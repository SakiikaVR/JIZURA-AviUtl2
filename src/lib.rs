use aviutl2::{AnyResult,AviUtl2Info};
use aviutl2::filter::*;
use aviutl2::generic::*;
use anyhow::{anyhow,bail,ensure};
use base64::{Engine,engine::general_purpose::STANDARD as B64};
use raw_window_handle::{HasWindowHandle,WindowHandle,RawWindowHandle,Win32WindowHandle};
use serde_json::Value;
use std::{collections::VecDeque,ffi::{c_char,c_void,CStr,CString},hash::{Hash,Hasher},path::PathBuf,sync::{Mutex,OnceLock,atomic::{AtomicI64,Ordering}},num::NonZeroIsize};

unsafe extern "C" {
    fn jz_init(directory:*const u16);
    fn jz_panel(callback:extern "C" fn(*const c_char))->*mut c_void;
    fn jz_render(snapshot:*const c_char,time:f64,transparent:bool)->*mut c_void;
    fn jz_pixels(frame:*mut c_void,w:*mut i32,h:*mut i32)->*const u8;
    fn jz_free_frame(frame:*mut c_void);
    fn jz_error()->*const c_char;
    fn jz_load(snapshot:*const c_char);
    fn jz_status(text:*const c_char);
    fn jz_exec(script:*const c_char);
    fn jz_sample();
    fn jz_preview_state(active:bool);
    fn jz_shutdown();
}
static EDIT:GlobalEditHandle=GlobalEditHandle::new();
static TARGET:AtomicI64=AtomicI64::new(0);
static NATIVE_FREE_LAYERS:Mutex<VecDeque<usize>>=Mutex::new(VecDeque::new());
static NATIVE_CUT_FREE_LAYERS:Mutex<VecDeque<usize>>=Mutex::new(VecDeque::new());
static NATIVE_CUT_SNAPSHOT:Mutex<Option<String>>=Mutex::new(None);
static NATIVE_CUT_PCM:Mutex<Option<(u32,usize,Vec<u8>)>>=Mutex::new(None);
static NATIVE_CUT_ORIGIN:Mutex<Option<usize>>=Mutex::new(None);
static NATIVE_OLD_SOURCE:Mutex<Option<i64>>=Mutex::new(None);
static BASE:OnceLock<PathBuf>=OnceLock::new();
static CATALOG:OnceLock<Value>=OnceLock::new();
static FONT_CATALOG:OnceLock<Vec<(String,String)>>=OnceLock::new();
const DEFAULT:&str="eyJ2ZXJzaW9uIjoxfQ==";
fn c(s:&str)->CString { CString::new(s.replace('\0',"")).unwrap() }
fn status(s:&str){unsafe{jz_status(c(s).as_ptr());}}
fn native_trace(s:&str){
    if let Some(base)=BASE.get(){
        if let Ok(mut file)=std::fs::OpenOptions::new().create(true).append(true).open(base.join("JIZURA-native.log")){
            use std::io::Write;
            let _=writeln!(file,"{s}");
        }
    }
}
fn native_ack(done:u64,total:u64,error:Option<&str>){
    let e=match error {Some(s)=>serde_json::to_string(s).unwrap_or_else(|_|"\"native error\"".into()),None=>"null".into()};
    let script=format!("window.aviNativeAck({done},{total},{e})");unsafe{jz_exec(c(&script).as_ptr());}
}
fn common_bpm_ack(value:Option<f64>){
    let number=value.filter(|v|v.is_finite()&&*v>=0.&&*v<=300.).map_or("null".to_owned(),|v|v.to_string());
    let script=format!("window.aviCommonBpmAck({number})");unsafe{jz_exec(c(&script).as_ptr());}
}
fn directory()->PathBuf {
    // Resolve this DLL, not aviutl2.exe; helper's module address works with static linking.
    unsafe extern "system" {fn GetModuleHandleExW(flags:u32,address:*const u16,module:*mut *mut c_void)->i32;fn GetModuleFileNameW(module:*mut c_void,path:*mut u16,size:u32)->u32;}
    let mut module=std::ptr::null_mut();let mut path=vec![0u16;32768];
    unsafe{GetModuleHandleExW(6,directory as *const () as *const u16,&mut module);let n=GetModuleFileNameW(module,path.as_mut_ptr(),path.len() as u32);path.truncate(n as usize);}
    PathBuf::from(String::from_utf16_lossy(&path)).parent().unwrap().to_owned()
}
fn save_audio_ref(audio:&Value)->AnyResult<Option<String>> {
    if audio.is_null(){return Ok(None);}
    let bytes=serde_json::to_vec(audio)?;
    let mut hasher=std::collections::hash_map::DefaultHasher::new();bytes.hash(&mut hasher);
    let name=format!("JIZURA-audio-{:016x}.json",hasher.finish());
    let path=BASE.get().ok_or_else(||anyhow!("plugin directory missing"))?.join(&name);
    if !path.exists(){std::fs::write(path,bytes)?;}
    Ok(Some(name))
}
fn restore_audio_ref(encoded:&str)->AnyResult<String> {
    let mut snap:Value=serde_json::from_slice(&B64.decode(encoded)?)?;
    if snap["audioData"].is_null() {
        if let Some(name)=snap["audioRef"].as_str() {
            if name.starts_with("JIZURA-audio-")&&name.ends_with(".json")&&!name.contains('/')&&!name.contains('\\') {
                if let Some(base)=BASE.get() {
                    if let Ok(bytes)=std::fs::read(base.join(name)) {
                        if let Ok(audio)=serde_json::from_slice::<Value>(&bytes) {snap["audioData"]=audio;}
                    }
                }
            }
        }
    }
    Ok(B64.encode(serde_json::to_vec(&snap)?))
}
fn settings(config:&[FilterConfigItem])->(String,bool,f64){
    let mut s=DEFAULT.to_string();let mut tr=false;let mut t=0.;
    for item in config {match item {FilterConfigItem::String(v) if v.name=="編集データ"=>s=v.value.clone(),FilterConfigItem::Check(v) if v.name=="背景透過"=>tr=v.value,FilterConfigItem::Track(v) if v.name=="時間オフセット"=>t=v.value,_=>{}}}
    (s,tr,t)
}
const CUT_SELECT_ITEMS:[(&str,&str);11]=[("レイアウト","layout"),("登場","enter"),("保持","hold"),("退場","exit"),("装飾","decor"),("装飾2","decor"),("装飾3","decor"),("加工","treat"),("背景","bg"),("カメラ","cam"),("つなぎ","trans")];
const FX_ITEMS:[(&str,&str);7]=[("動きの強さ","motion"),("グリッチ","glitch"),("色ズレ","chroma"),("装飾の量","decor"),("カットの細かさ","density"),("質感","texture"),("背景の切替","bgSwitch")];
const COLOR_ITEMS:[(&str,&str);6]=[("背景色","bg"),("文字色","fg"),("補助色","sub"),("アクセント色","accent"),("ズレ色A","ghostA"),("ズレ色B","ghostB")];
const FONT_ITEMS:[(&str,&str);3]=[("見出しフォント","display"),("明朝枠フォント","serif"),("小さな文字フォント","body")];
fn font_entries()->&'static [(String,String)] {
    FONT_CATALOG.get_or_init(||{
        let data:Value=serde_json::from_str(include_str!("../upstream/ae/data.json")).expect("invalid font catalog");
        data["fonts"].as_object().into_iter().flat_map(|o|o.iter()).filter_map(|(key,v)|Some((key.clone(),v["label"].as_str()?.to_owned()))).collect()
    })
}
fn font_items()->Vec<FilterConfigSelectItem> {
    let mut items=vec![FilterConfigSelectItem{name:"スタイルの既定".into(),value:0}];
    items.extend(font_entries().iter().enumerate().map(|(i,(_,name))|FilterConfigSelectItem{name:name.clone(),value:i as i32+1}));items
}
fn font_index(key:&str)->i32 {font_entries().iter().position(|(k,_)|k==key).map_or(0,|i|i as i32+1)}
fn font_key(index:i32)->Option<String> {if index<1{None}else{font_entries().get((index-1) as usize).map(|(key,_)|key.clone())}}
fn color_code(raw:&str)->u32 {let s=raw.trim().trim_start_matches('#');u32::from_str_radix(s,16).unwrap_or(0)&0xFFFFFF}
fn catalog()->&'static Value {CATALOG.get_or_init(||serde_json::from_str(include_str!("../catalog.json")).expect("invalid JIZURA catalog"))}
fn catalog_entries(group:&str)->Vec<(String,String)>{
    catalog()["groups"][group].as_array().into_iter().flatten().filter_map(|v|Some((v["key"].as_str()?.to_owned(),v["name"].as_str()?.to_owned()))).collect()
}
fn select_items(group:&str)->Vec<FilterConfigSelectItem>{
    let mut items=vec![FilterConfigSelectItem{name:"元の設定".into(),value:0}];
    if group=="decor"||group=="trans"{items.push(FilterConfigSelectItem{name:"なし".into(),value:1});}
    let base=items.len() as i32;
    items.extend(catalog_entries(group).into_iter().enumerate().map(|(i,(_,name))|FilterConfigSelectItem{name,value:base+i as i32}));items
}
fn select_index(group:&str,raw:&str)->i32{
    if raw.is_empty(){return 0;}
    if (group=="decor"||group=="trans")&&raw=="なし"{return 1;}
    let base=if group=="decor"||group=="trans"{2}else{1};
    catalog_entries(group).iter().position(|(key,name)|key==raw||name==raw).map_or(0,|i|base+i as i32)
}
fn selected_key(group:&str,index:i32)->Option<String>{
    if index<=0{return None;}
    if (group=="decor"||group=="trans")&&index==1{return Some("なし".into());}
    let base=if group=="decor"||group=="trans"{2}else{1};
    catalog_entries(group).get((index-base) as usize).map(|(key,_)|key.clone())
}
fn edit_choice_value(edits:&Value,name:&str,group:&str)->i32{
    if group=="decor"{
        let position=match name{"装飾2"=>1,"装飾3"=>2,_=>0};
        let value=edits["decor"].as_array().and_then(|v|v.get(position)).and_then(Value::as_str).unwrap_or(if position==0{"なし"}else{""});
        return select_index(group,value);
    }
    select_index(group,edits[group].as_str().unwrap_or(""))
}
fn cut_edit_values(config:&[FilterConfigItem])->Value {
    let mut edit=serde_json::Map::new();
    let mut decor=Vec::new();let mut decor_selected=false;
    let mut fx=serde_json::Map::new();let mut colors=serde_json::Map::new();let mut fonts=serde_json::Map::new();
    for item in config {match item {
        FilterConfigItem::Text(v) if v.name=="カット文字"=>{if !v.value.is_empty(){edit.insert("text".into(),Value::String(v.value.clone()));}},
        FilterConfigItem::Select(v)=>{if let Some((_,role))=FONT_ITEMS.iter().find(|(name,_)|*name==v.name){
            if let Some(key)=font_key(v.value){fonts.insert((*role).into(),Value::String(key));}
        }else if let Some((_,group))=CUT_SELECT_ITEMS.iter().find(|(name,_)|*name==v.name){
            if let Some(key)=selected_key(group,v.value){if *group=="decor"{decor_selected=true;if key!="なし"{decor.push(key);}}else{edit.insert((*group).into(),Value::String(key));}}
        }},
        FilterConfigItem::Track(v)=>{if let Some((_,key))=FX_ITEMS.iter().find(|(name,_)|*name==v.name){fx.insert((*key).into(),Value::from(v.value.clamp(0.,1.)));}},
        FilterConfigItem::Color(v)=>{if let Some((_,key))=COLOR_ITEMS.iter().find(|(name,_)|*name==v.name){colors.insert((*key).into(),Value::String(format!("#{:06X}",v.value.0&0xFFFFFF)));}},
        FilterConfigItem::Check(v)=>{match v.name.as_str(){"フラッシュ"=>{fx.insert("flash".into(),Value::Bool(v.value));},"背景・文字色を指定"=>{colors.insert("enabled".into(),Value::Bool(v.value));},"アクセント色を指定"=>{colors.insert("accentOn".into(),Value::Bool(v.value));},_=>{}}},
        _=>{}
    }}
    if decor_selected{edit.insert("decor".into(),Value::String(if decor.is_empty(){"なし".into()}else{decor.join(",")}));}
    if !fx.is_empty(){edit.insert("fx".into(),Value::Object(fx));}
    if !colors.is_empty(){edit.insert("colors".into(),Value::Object(colors));}
    if !fonts.is_empty(){edit.insert("fonts".into(),Value::Object(fonts));}
    Value::Object(edit)
}
struct CachedFrame { snapshot:String,time:u64,transparent:bool,width:u32,height:u32,pixels:Vec<u8> }
extern "C" fn native_apply_button(section:*mut aviutl2::sys::plugin2::EDIT_SECTION,object:*mut c_void,_name:*const u16,_value:*const u16){
    let result=std::panic::catch_unwind(||->AnyResult<()> {
        ensure!(!section.is_null()&&!object.is_null(),"カットを選択してください");
        let e=unsafe{EditSection::from_raw(section)};
        let ob=ObjectHandle::from(object);
        ensure!(e.count_object_effect(ob,"JIZURA")?>0,"JIZURAのカットを選択してください");
        let mut snapshot=e.get_object_effect_item(ob,"JIZURA",0,"編集データ")?;
        let owner=B64.decode(&snapshot).ok().and_then(|bytes|serde_json::from_slice::<Value>(&bytes).ok()).and_then(|snap|snap["nativeOwner"].as_i64());
        if let Some(owner_id)=owner {
            'owner: for layer in 0..=e.info.layer_max {let mut frame=0;while let Some(other)=e.find_object_after(layer,frame)? {
                if e.get_object_id(other)?==owner_id {snapshot=e.get_object_effect_item(other,"JIZURA",0,"編集データ")?;break 'owner;}
                let lf=e.get_object_layer_frame(other)?;if lf.end<frame{break;}frame=lf.end.saturating_add(1);
            }}
        }
        TARGET.store(owner.unwrap_or(e.get_object_id(ob)?),Ordering::SeqCst);
        snapshot=restore_audio_ref(&snapshot)?;
        let mut common=serde_json::json!({"fx":{},"colors":{},"fonts":{}});
        if let Ok(bpm)=e.get_object_effect_item(ob,"JIZURA",0,"共通BPM（適用で反映）").unwrap_or_default().parse::<f64>() {
            if bpm.is_finite()&&bpm>=0.&&bpm<=300. {common["bpm"]=Value::from(bpm);}
        }
        for (name,key) in FX_ITEMS {
            if let Ok(value)=e.get_object_effect_item(ob,"JIZURA",0,name).unwrap_or_default().parse::<f64>() {
                if value.is_finite()&&value>=0.&&value<=1. {common["fx"][key]=Value::from(value);}
            }
        }
        let flash=e.get_object_effect_item(ob,"JIZURA",0,"フラッシュ").unwrap_or_default();
        if !flash.is_empty(){common["fx"]["flash"]=Value::Bool(flash=="true"||flash=="1");}
        for (name,key) in COLOR_ITEMS {
            let value=e.get_object_effect_item(ob,"JIZURA",0,name).unwrap_or_default();
            if !value.is_empty(){common["colors"][key]=Value::String(format!("#{:06X}",color_code(&value)));}
        }
        for (name,key) in [("背景・文字色を指定","enabled"),("アクセント色を指定","accentOn")] {
            let value=e.get_object_effect_item(ob,"JIZURA",0,name).unwrap_or_default();
            if !value.is_empty(){common["colors"][key]=Value::Bool(value=="true"||value=="1");}
        }
        for (name,role) in FONT_ITEMS {
            let value=e.get_object_effect_item(ob,"JIZURA",0,name).unwrap_or_default();
            if let Ok(index)=value.parse::<i32>() {common["fonts"][role]=font_key(index).map_or(Value::Null,Value::String);}
        }
        let script=format!("window.aviLoadAndApply({},{})",serde_json::to_string(&snapshot)?,common);
        unsafe{jz_exec(c(&script).as_ptr());}
        Ok(())
    });
    match result {Ok(Err(e))=>status(&format!("JIZURA: {e:#}")),Err(_)=>status("JIZURA: 適用中にエラーが発生しました"),_=>{}}
}
#[aviutl2::plugin(FilterPlugin)]
pub struct JizuraFilter { cache:Mutex<Option<CachedFrame>>,audio:Mutex<Option<(String,u32,usize,Vec<f32>)>> }
impl FilterPlugin for JizuraFilter {
    type Userdata=();
    fn new(_:AviUtl2Info)->AnyResult<Self>{Ok(Self{cache:Mutex::new(None),audio:Mutex::new(None)})}
    fn plugin_info(&self)->FilterPluginTable{
        let mut config_items=vec![
            FilterConfigItem::Check(FilterConfigCheck{name:"背景透過".into(),value:false}),
            FilterConfigItem::Track(FilterConfigTrack{name:"時間オフセット".into(),value:0.,range:-3600.0..=3600.0,step:0.001,zero_display:None,slider_ratio:1.}),
            FilterConfigItem::Text(FilterConfigText{name:"カット文字".into(),value:String::new()}),
        ];
        config_items.extend(CUT_SELECT_ITEMS.into_iter().map(|(name,group)|FilterConfigItem::Select(FilterConfigSelect{name:name.into(),value:0,items:select_items(group)})));
        config_items.extend(FONT_ITEMS.into_iter().map(|(name,_)|FilterConfigItem::Select(FilterConfigSelect{name:name.into(),value:0,items:font_items()})));
        config_items.push(FilterConfigItem::Check(FilterConfigCheck{name:"背景・文字色を指定".into(),value:false}));
        config_items.push(FilterConfigItem::Check(FilterConfigCheck{name:"アクセント色を指定".into(),value:false}));
        config_items.extend(COLOR_ITEMS.into_iter().map(|(name,_)|FilterConfigItem::Color(FilterConfigColor{name:name.into(),value:FilterConfigColorValue(0xFFFFFF)})));
        config_items.extend(FX_ITEMS.into_iter().map(|(name,_)|FilterConfigItem::Track(FilterConfigTrack{name:name.into(),value:0.5,range:0.0..=1.0,step:0.01,zero_display:None,slider_ratio:1.})));
        config_items.push(FilterConfigItem::Check(FilterConfigCheck{name:"フラッシュ".into(),value:true}));
        config_items.push(FilterConfigItem::Track(FilterConfigTrack{name:"共通BPM（適用で反映）".into(),value:0.0,range:0.0..=300.0,step:0.1,zero_display:Some("自動".into()),slider_ratio:1.}));
        config_items.push(FilterConfigItem::Button(FilterConfigButton{name:"適用".into(),callback:native_apply_button}));
        config_items.extend([
            FilterConfigItem::String(FilterConfigString{name:"編集データ".into(),value:DEFAULT.into()}),
            FilterConfigItem::HideRule(FilterConfigHideRule{name:"編集データ".into(),condition_name:None,condition_operator:FilterConfigHideRuleOperator::Equal,condition_value:0}),
        ]);
        FilterPluginTable{name:"JIZURA".into(),label:Some("JIZURA".into()),information:"JIZURA 0.3 / aviutl2-rs / original (c) 2026 hakoniwa".into(),flags:aviutl2::bitflag!(FilterPluginFlags{video:true,audio:true,input:true}),config_items}
    }
    fn proc_video(&self,config:&[FilterConfigItem],v:&mut FilterProcVideo<()>)->AnyResult<()> {
        let(s,tr,offset)=settings(config);let time=(v.object.time+offset).max(0.);
        let edits=cut_edit_values(config);
        let cache_key=format!("{s}|{}",serde_json::to_string(&edits)?);
        let mut cache=self.cache.lock().map_err(|_|anyhow!("render lock poisoned"))?;
        if !cache.as_ref().is_some_and(|x|x.snapshot==cache_key&&x.time==time.to_bits()&&x.transparent==tr){
            // PCM and original sound bytes are not needed by the visual engine.
            let mut snap:Value=serde_json::from_slice(&B64.decode(&s)?)?;
            if let Some(o)=snap.as_object_mut(){o.remove("pcm");o.remove("audioData");}
            if snap["nativeCut"].is_number(){snap["nativeEdit"]=edits;}
            let visual=B64.encode(serde_json::to_vec(&snap)?);
            unsafe {
                let frame=jz_render(c(&visual).as_ptr(),time,tr);
                if frame.is_null(){bail!("{}",CStr::from_ptr(jz_error()).to_string_lossy());}
                let(mut w,mut h)=(0,0);let pixels=jz_pixels(frame,&mut w,&mut h);
                ensure!(w>0&&h>0&&w<=8192&&h<=8192,"invalid JIZURA frame size");
                let source=std::slice::from_raw_parts(pixels,w as usize*h as usize*4);
                let data=source.to_vec();
                jz_free_frame(frame);
                *cache=Some(CachedFrame{snapshot:cache_key,time:time.to_bits(),transparent:tr,width:w as u32,height:h as u32,pixels:data});
            }
        }
        let x=cache.as_ref().unwrap();v.set_image_data(&x.pixels,x.width,x.height);Ok(())
    }
    fn proc_audio(&self,config:&[FilterConfigItem],a:&mut FilterProcAudio<()>)->AnyResult<()> {
        let(s,_,offset)=settings(config);let mut cache=self.audio.lock().map_err(|_|anyhow!("audio lock poisoned"))?;
        if !cache.as_ref().is_some_and(|x|x.0==s){
            let snap:Value=serde_json::from_slice(&B64.decode(&s)?)?;let pcm=&snap["pcm"];
            let rate=pcm["rate"].as_u64().unwrap_or(0) as u32;let channels=pcm["channels"].as_u64().unwrap_or(0) as usize;
            let bytes=B64.decode(pcm["data"].as_str().unwrap_or(""))?;
            ensure!(channels<=2,"invalid channel count");
            let data=if pcm["format"]=="s16" {
                bytes.chunks_exact(2).map(|b|i16::from_le_bytes(b.try_into().unwrap()) as f32/32768.).collect()
            } else {bytes.chunks_exact(4).map(|b|f32::from_le_bytes(b.try_into().unwrap())).collect()};
            *cache=Some((s,rate,channels,data));
        }
        let(_,rate,channels,data)=cache.as_ref().unwrap();
        for(channel,c) in [(AudioChannel::Left,0),(AudioChannel::Right,1)] {
            let mut out=vec![0f32;a.audio_object.sample_num as usize];
            if *channels>0&&*rate>0 {let count=data.len()/channels;
                for(i,v) in out.iter_mut().enumerate(){let pos=((a.audio_object.sample_index+i as u64) as f64/a.scene.sample_rate as f64+offset)* *rate as f64;let n=pos.floor() as i64;
                    if n>=0&&(n as usize)<count {let c=c.min(channels-1);let x=data[n as usize*channels+c];let y=data.get((n as usize+1)*channels+c).copied().unwrap_or(0.);*v=x+(y-x)*(pos-n as f64) as f32;}
                }
            }a.set_sample_data(channel,&out);
        }Ok(())
    }
}
struct NativeWindow(isize);
impl HasWindowHandle for NativeWindow{fn window_handle(&self)->Result<WindowHandle<'_>,raw_window_handle::HandleError>{let raw=Win32WindowHandle::new(NonZeroIsize::new(self.0).unwrap());Ok(unsafe{WindowHandle::borrow_raw(RawWindowHandle::Win32(raw))})}}
#[aviutl2::plugin(GenericPlugin)]
pub struct JizuraPlugin{window:NativeWindow,filter:SubPlugin<JizuraFilter>}
impl GenericPlugin for JizuraPlugin{
    fn new(info:AviUtl2Info)->AnyResult<Self>{
        let base=directory();let _=BASE.set(base.clone());let path:Vec<u16>=base.to_string_lossy().encode_utf16().chain(Some(0)).collect();
        unsafe{jz_init(path.as_ptr());}
        std::panic::set_hook(Box::new(|info| {
            if let Some(base)=BASE.get() {
                if let Ok(mut file)=std::fs::OpenOptions::new().create(true).append(true).open(base.join("JIZURA-panic.log")) {
                    use std::io::Write;
                    let _=writeln!(file,"{info}\n{}",std::backtrace::Backtrace::force_capture());
                }
            }
        }));
        let window=NativeWindow(unsafe{jz_panel(on_message)} as isize);ensure!(window.0!=0,"Cannot create JIZURA panel");
        Ok(Self{window,filter:SubPlugin::new_filter_plugin(&info)?})
    }
    fn plugin_info(&self)->GenericPluginTable{GenericPluginTable{name:"JIZURA".into(),information:"JIZURA 0.3 — aviutl2-rs + original JIZURA editor/renderer".into()}}
    fn register(&mut self,r:&mut HostAppHandle){
        EDIT.init(r.create_edit_handle());r.register_filter_plugin(&self.filter);
        if let Err(e)=r.register_window_client("JIZURA",&self.window){status(&e.to_string());}
        r.register_import_menu("JIZURA サンプルプロジェクト",||unsafe{jz_sample();});
        r.register_object_menu("JIZURAで編集",||{let result=EDIT.call_edit_section(|e|->AnyResult<()>{
            if let Some(ob)=e.get_focused_object()?{if e.count_object_effect(ob,"JIZURA")?==0{return Ok(());}
                let mut s=e.get_object_effect_item(ob,"JIZURA",0,"編集データ")?;
                let owner=B64.decode(&s).ok().and_then(|bytes|serde_json::from_slice::<Value>(&bytes).ok()).and_then(|snap|snap["nativeOwner"].as_i64());
                if let Some(owner_id)=owner {
                    'owner: for layer in 0..=e.info.layer_max {let mut frame=0;while let Some(other)=e.find_object_after(layer,frame)? {
                        if e.get_object_id(other)?==owner_id {s=e.get_object_effect_item(other,"JIZURA",0,"編集データ")?;break 'owner;}
                        let lf=e.get_object_layer_frame(other)?;if lf.end<frame{break;}frame=lf.end.saturating_add(1);
                    }}
                }
                TARGET.store(owner.unwrap_or(e.get_object_id(ob)?),Ordering::SeqCst);s=restore_audio_ref(&s)?;unsafe{jz_load(c(&s).as_ptr());}}
            Ok(())});if let Err(e)=result{status(&e.to_string());}});
    }
    fn on_project_load(&mut self,_:&mut ProjectFile){TARGET.store(0,Ordering::SeqCst);}
    fn event_change_edit_state(&mut self){
        let state=if EDIT.is_ready(){match EDIT.get_edit_state(){Ok(EditState::Preview)=>1,Ok(EditState::Save)=>2,_=>0}}else{0};
        unsafe{jz_preview_state(state==1);}
        native_trace(&format!("edit state: {state}"));
    }
}
impl Drop for JizuraPlugin{fn drop(&mut self){unsafe{jz_shutdown();}}}
extern "C" fn on_message(raw:*const c_char){
    let mut native_progress=None;
    let result=std::panic::catch_unwind(std::panic::AssertUnwindSafe(||->AnyResult<()>{
        let m:Value=serde_json::from_str(unsafe{CStr::from_ptr(raw)}.to_str()?)?;
        if m["type"]=="native" || m["type"]=="cut" || m["type"]=="mode" || m["type"]=="sample" {native_progress=Some((m["done"].as_u64().unwrap_or(0),m["total"].as_u64().unwrap_or(1)));}
        match m["type"].as_str().unwrap_or("") {
            "sample"=>{apply_sample(&m)?;native_ack(1,1,None);},
            "native"=>apply_native(&m)?,
            "cut"=>apply_cut(&m)?,
            "mode"=>apply_mode(&m)?,
            "commonBpm"=>read_common_bpm()?,
            "new"=>{TARGET.store(0,Ordering::SeqCst);},
            "error"=>status(m["error"].as_str().unwrap_or("Unknown error")),
            "saveSample"=>{let path=BASE.get().unwrap().join("sample-output.txt");if path.exists(){let dest=std::fs::read_to_string(path)?;EDIT.save_project_file(PathBuf::from(dest.trim()).as_path())?;status("サンプルプロジェクトを保存しました");}},
            _=>{}
        }Ok(())
    }));
    match result{Ok(Err(e))=>{status(&format!("JIZURA: {e:#}"));if let Some((done,total))=native_progress{native_ack(done,total,Some(&format!("{e:#}")));}},Err(_)=>{status("JIZURA: internal panic caught");if let Some((done,total))=native_progress{native_ack(done,total,Some("内部エラーが発生しました"));}},_=>{}}
}
fn read_common_bpm()->AnyResult<()> {
    let target_id=TARGET.load(Ordering::SeqCst);
    if target_id==0 {common_bpm_ack(None);return Ok(());}
    let bpm=EDIT.call_edit_section(|e|->AnyResult<Option<f64>> {
        if let Some(ob)=e.get_focused_object()? {
            if e.count_object_effect(ob,"JIZURA")?>0 {
                let encoded=e.get_object_effect_item(ob,"JIZURA",0,"編集データ").unwrap_or_default();
                let owner=B64.decode(encoded).ok().and_then(|bytes|serde_json::from_slice::<Value>(&bytes).ok()).and_then(|snap|snap["nativeOwner"].as_i64());
                if owner==Some(target_id) {
                    let value=e.get_object_effect_item(ob,"JIZURA",0,"共通BPM（適用で反映）").unwrap_or_default();
                    if let Ok(bpm)=value.parse::<f64>() {return Ok(Some(bpm));}
                }
            }
        }
        if target_id!=0 {
            for layer in 0..=e.info.layer_max {let mut frame=0;while let Some(ob)=e.find_object_after(layer,frame)? {
                if e.get_object_id(ob)?==target_id {
                    let value=e.get_object_effect_item(ob,"JIZURA",0,"共通BPM（適用で反映）").unwrap_or_default();
                    return Ok(value.parse::<f64>().ok());
                }
                let lf=e.get_object_layer_frame(ob)?;if lf.end<frame{break;}frame=lf.end.saturating_add(1);
            }}
        }
        Ok(None)
    })??;
    common_bpm_ack(bpm);
    Ok(())
}
fn apply_sample(m:&Value)->AnyResult<()>{
    let w=m["w"].as_u64().unwrap_or(0) as u32;let h=m["h"].as_u64().unwrap_or(0) as u32;let fps=m["fps"].as_u64().unwrap_or(0) as i32;let duration=m["duration"].as_f64().unwrap_or(0.);
    ensure!((1..=8192).contains(&w)&&(1..=8192).contains(&h)&&(1..=240).contains(&fps)&&duration>0.&&duration<86400.,"invalid dimensions or duration");
    EDIT.create_project(w,h,aviutl2::Rational32::new(fps,1),48000,Some((0,0,0)),true)?;
    TARGET.store(0,Ordering::SeqCst);
    Ok(())
}

fn apply_native(m:&Value)->AnyResult<()> {
    let final_batch=m["final"].as_bool().unwrap_or(true);
    // Keep text generation in small edit sections; large bursts have caused
    // instability in AviUtl2.
    let render_snapshot:Option<String>=None;
    let cuts=m["cuts"].as_array().ok_or_else(||anyhow!("missing cuts"))?;
    let mut glyph_count=0usize;
    for cut in cuts {
        let n=cut["chars"].as_array().map_or(0,Vec::len);
        glyph_count=glyph_count.saturating_add(n);
    }
    ensure!(glyph_count>0,"ネイティブ化する歌詞がありません");
    ensure!(glyph_count<=256,"一回の処理文字数が多すぎます");
    ensure!(m["total"].as_u64().unwrap_or(glyph_count as u64)<=4096,"文字数が多すぎます（上限 4096 文字）");
    let w=m["w"].as_f64().unwrap_or(1920.);let h=m["h"].as_f64().unwrap_or(1080.);
    ensure!(w>0.&&h>0.,"invalid native project size");
    let target_id=TARGET.load(Ordering::SeqCst);
    let base_frame=m["baseFrame"].as_u64().unwrap_or(0) as usize;
    let done_count=m["done"].as_u64().unwrap_or(glyph_count as u64);
    let total_count=m["total"].as_u64().unwrap_or(glyph_count as u64);
    native_trace(&format!("batch start: {done_count}/{total_count}"));
    // The SDK invokes this closure through a non-unwinding C callback. Catch
    // Rust panics inside the closure so they become a reported generation
    // error instead of aborting AviUtl2.
    let result=EDIT.call_edit_section(move |e| {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(||->AnyResult<()> {
        let fps=*e.info.fps.numer() as f64 / *e.info.fps.denom() as f64;
        ensure!(fps>0.,"invalid scene frame rate");
        let mut source=None;
        if target_id!=0 {
            'layers: for layer in 0..=e.info.layer_max {
                let mut frame=0;
                while let Some(ob)=e.find_object_after(layer,frame)? {
                    if e.get_object_id(ob)?==target_id {source=Some(ob);break 'layers;}
                    let lf=e.get_object_layer_frame(ob)?;if lf.end<frame{break;}frame=lf.end+1;
                }
            }
        }
        ensure!(source.is_some(),"先に『AviUtl2に適用』で映像オブジェクトを作成してください");
        let origin=source.map(|ob|e.get_object_layer_frame(ob).map(|lf|lf.start)).transpose()?.unwrap_or(if base_frame>0 {base_frame}else{e.info.frame});
        let mut created=0usize;
        let first_layer=e.info.layer;
        let mut next_new_layer=e.info.layer_max.saturating_add(1);
        let scene_scale=e.info.height as f64/h;
        if m["first"].as_bool().unwrap_or(false) {
            native_trace("cleanup start");
            for layer in 0..=e.info.layer_max {
                let mut frame=0;
                while let Some(ob)=e.find_object_after(layer,frame)? {
                    let name=e.get_object_name(ob)?.unwrap_or_default();
                    let generated=name.starts_with("JZ Glyph C") || name.starts_with("JZ C") && name.contains(" G");
                    let lf=e.get_object_layer_frame(ob)?;
                    if generated { e.delete_object(ob)?; e.set_layer_enable(layer,true)?; }
                    if lf.end<frame { break; }
                    frame=lf.end.saturating_add(1);
                }
            }
            let mut free=NATIVE_FREE_LAYERS.lock().map_err(|_|anyhow!("native layer pool lock poisoned"))?;
            free.clear();
            for layer in 0..=e.info.layer_max {
                let name=e.get_layer_name(layer)?.unwrap_or_default();
                let generated=name.starts_with("JZ Text ");
                if generated && e.find_object_after(layer,0)?.is_none(){free.push_back(layer);}
            }
            native_trace("cleanup done");
            if m["hud"].as_bool().unwrap_or(false) {
                if let Some(encoded)=render_snapshot.as_deref() {
                    let mut hud_snap:Value=serde_json::from_slice(&B64.decode(encoded)?)?;
                    hud_snap["nativeHud"]=Value::Bool(true);
                    let hud_data=B64.encode(serde_json::to_vec(&hud_snap)?);
                    let hud_len=(m["duration"].as_f64().unwrap_or(1.).max(1./fps)*fps).ceil() as usize;
                    let hud_end=origin.saturating_add(hud_len);
                    let mut hud=None;
                    // Query APIs are valid only for layers already present in
                    // this edit section. Speculative indices can corrupt AviUtl2.
                    for layer in 0..=e.info.layer_max {
                        let mut scan=0;let mut busy=false;
                        while let Some(other)=e.find_object_after(layer,scan)? {
                            let lf=e.get_object_layer_frame(other)?;
                            if lf.start<hud_end && lf.end.saturating_add(1)>origin {busy=true;break;}
                            if lf.end<scan {break;}scan=lf.end.saturating_add(1);
                        }
                        if !busy {
                            if let Ok(ob)=e.create_object("JIZURA",layer,origin,Some(hud_len)) {
                                e.set_object_effect_item(ob,"JIZURA",0,"編集データ",&hud_data)?;
                                e.set_object_effect_item(ob,"JIZURA",0,"背景透過","true")?;
                                e.set_object_name(ob,Some("JZ HUD Overlay"))?;
                                if e.get_layer_name(layer)?.is_none() {let _=e.set_layer_name(layer,Some("JZ HUD Overlay"));}hud=Some(ob);break;
                            }
                        }
                    }
                    ensure!(hud.is_some(),"HUDレイヤーを配置できませんでした");
                }
            }
        }
        for (ci,cut) in cuts.iter().enumerate() {
            let chars=cut["chars"].as_array().ok_or_else(||anyhow!("invalid glyph list"))?;
            let adv=cut["advances"].as_array();
            let mut widths=Vec::with_capacity(chars.len());
            for (i,ch) in chars.iter().enumerate() {
                let a=adv.and_then(|a|a.get(i)).and_then(Value::as_f64).unwrap_or(1.).clamp(0.25,2.5);
                widths.push((ch.as_str().unwrap_or(""),a));
            }
            let total: f64=widths.iter().map(|(_,a)|a).sum::<f64>().max(1.);
            let cut_w=w;let cut_h=h;
            let size=(cut_h*0.19).min(cut_w*0.84/total).clamp(12.,500.);
            let vertical=matches!(cut["layout"].as_str().unwrap_or(""),"vcols") || cut["layout"].as_str().unwrap_or("").contains("vertical");
            let text_extent=if vertical { total*size } else { total*size };
            let start=origin+(cut["start"].as_f64().unwrap_or(0.).max(0.)*fps).round() as usize;
            let end=origin+(cut["end"].as_f64().unwrap_or(0.).max(0.)*fps).round() as usize;
            if end<=start {continue;}
            let color=cut["color"].as_str().unwrap_or("ffffff").trim_start_matches('#');
            let font=cut["font"].as_str().unwrap_or("Yu Gothic");
            let mut pen=-text_extent*0.5;
            let cut_index=cut["index"].as_u64().unwrap_or(ci as u64) as usize+1;
            let glyph_offset=cut["glyphOffset"].as_u64().unwrap_or(0) as usize;
            let glyph_total=cut["glyphTotal"].as_u64().unwrap_or(chars.len() as u64) as usize;
            if glyph_offset==0 {
                if let Some(encoded)=render_snapshot.as_deref() {
                    let mut render_snap:Value=serde_json::from_slice(&B64.decode(encoded)?)?;
                    render_snap["nativeCut"]=Value::from(cut_index-1);
                    render_snap["nativeCutDuration"]=cut["nativeDuration"].clone();
                    let render_data=B64.encode(serde_json::to_vec(&render_snap)?);
                    let object_start=origin+(cut["start"].as_f64().unwrap_or(0.).max(0.)*fps).round() as usize;
                    let object_len=(cut["nativeDuration"].as_f64().unwrap_or(cut["end"].as_f64().unwrap_or(0.)-cut["start"].as_f64().unwrap_or(0.)).max(1./fps)*fps).ceil() as usize;
                    let mut preview=None;
                    for layer in first_layer..=e.info.layer_max {
                        let mut scan=0;let mut busy=false;let mut has_glyph_track=false;
                        while let Some(other)=e.find_object_after(layer,scan)? {
                            let other_name=e.get_object_name(other)?.unwrap_or_default();
                            if other_name.starts_with("JZ Glyph ") {has_glyph_track=true;}
                            let lf=e.get_object_layer_frame(other)?;
                            if lf.start<object_start+object_len && lf.end.saturating_add(1)>object_start {busy=true;break;}
                            if lf.end<scan {break;}scan=lf.end.saturating_add(1);
                        }
                        if busy||has_glyph_track {continue;}
                        if let Ok(ob)=e.create_object("JIZURA",layer,object_start,Some(object_len)) {
                            e.set_object_effect_item(ob,"JIZURA",0,"編集データ",&render_data)?;
                            let name=format!("JZ Cut Visual {:03}",cut_index);
                            e.set_object_name(ob,Some(&name))?;
                            if e.get_layer_name(layer)?.is_none() {let _=e.set_layer_name(layer,Some(&format!("JZ Cut Visual Track {:03}",cut_index)));}
                            preview=Some(ob);break;
                        }
                    }
                    ensure!(preview.is_some(),"カット映像レイヤーを配置できませんでした（カット {}）",cut_index);
                    created+=1;
                }
            }
            for (gi,(glyph,advance)) in widths.iter().enumerate() {
                let is_space=glyph.trim().is_empty();
                let x=if vertical { -size*0.65 } else { pen+advance*size*0.5 };
                let y=if vertical { pen+advance*size*0.5 } else { 0. };
                pen+=advance*size;
                if is_space || glyph.is_empty(){continue;}
                let stagger=((cut["end"].as_f64().unwrap_or(0.)-cut["start"].as_f64().unwrap_or(0.)).max(0.)*0.12)
                    .min(0.16)*(glyph_offset+gi) as f64/(glyph_total.saturating_sub(1).max(1) as f64);
                let frame=start+(stagger*fps).round() as usize;
                let length=end.saturating_sub(frame).max(1);
                let px=x*w/cut_w*scene_scale;let py=y*h/cut_h*scene_scale;
                let native_size=size*scene_scale;
                let alias=format!("[Object]\r\n[Object.0]\r\neffect.name=テキスト\r\nサイズ={native_size:.2}\r\n文字色={color}\r\nフォント={font}\r\nテキスト={glyph}\r\n[Object.1]\r\neffect.name=標準描画\r\nX={px:.2}\r\nY={py:.2}\r\n");
                let mut placed=None;
                if placed.is_none() {
                    // The SDK allows insertion at the next layer. Do not call
                    // find_object on an index that does not exist yet.
                    let reused=NATIVE_FREE_LAYERS.lock().map_err(|_|anyhow!("native layer pool lock poisoned"))?.pop_front();
                    let layer=reused.unwrap_or(next_new_layer);
                    native_trace(&format!("glyph append: cut={cut_index} glyph={} layer={layer}",glyph_offset+gi+1));
                    if let Ok(ob)=e.create_object_from_alias(&alias,layer,frame,length) {
                        native_trace("glyph appended");
                        let name=format!("JZ Glyph C{:03} G{:03} {}",cut_index,glyph_offset+gi+1,glyph);
                        e.set_object_name(ob,Some(&name))?;
                        let _=e.set_layer_enable(layer,false);
                        let lname=format!("JZ Text C{:03} G{:03}",cut_index,glyph_offset+gi+1);
                        let _=e.set_layer_name(layer,Some(&lname));
                        if reused.is_none(){next_new_layer=next_new_layer.saturating_add(1);}
                        placed=Some(ob);
                    }
                }
                ensure!(placed.is_some(),"文字レイヤーを配置できませんでした（カット {}、文字 {}）",cut_index,glyph_offset+gi+1);
                created+=1;
            }
        }
        ensure!(created>0||!final_batch,"ネイティブテキストを作成できませんでした");
        if !final_batch {
            return Ok(());
        }
        // The visual carrier was applied before generation. Updating its
        // filter data here while dozens of new layers are pending causes
        // AviUtl2 to recalculate the whole scene and block the edit callback.
        native_trace("final glyph batch complete");
        Ok(())
        })).unwrap_or_else(|panic| {
            let detail=panic.downcast_ref::<String>().cloned()
                .or_else(||panic.downcast_ref::<&'static str>().map(|s|(*s).to_owned()))
                .unwrap_or_else(||"unknown Rust panic".to_owned());
            Err(anyhow!("panic inside AviUtl2 edit section: {detail}"))
        })
    });
    result??;
    native_trace(&format!("batch done: {done_count}/{total_count}"));
    native_ack(done_count,total_count,None);
    Ok(())
}
fn apply_cut(m:&Value)->AnyResult<()> {
    let first=m["first"].as_bool().unwrap_or(false);
    if first {
        *NATIVE_CUT_SNAPSHOT.lock().map_err(|_|anyhow!("cut snapshot lock poisoned"))?=
            Some(m["cutSnapshot"].as_str().ok_or_else(||anyhow!("missing cut snapshot"))?.to_owned());
        let pcm=&m["pcm"];
        let rate=pcm["rate"].as_u64().unwrap_or(0) as u32;
        let channels=pcm["channels"].as_u64().unwrap_or(0) as usize;
        ensure!(channels<=2,"invalid audio channel count");
        let bytes=B64.decode(pcm["data"].as_str().unwrap_or(""))?;
        *NATIVE_CUT_PCM.lock().map_err(|_|anyhow!("cut audio lock poisoned"))?=Some((rate,channels,bytes));
        *NATIVE_CUT_ORIGIN.lock().map_err(|_|anyhow!("cut origin lock poisoned"))?=None;
        *NATIVE_OLD_SOURCE.lock().map_err(|_|anyhow!("old source lock poisoned"))?=None;
    }
    let encoded=NATIVE_CUT_SNAPSHOT.lock().map_err(|_|anyhow!("cut snapshot lock poisoned"))?
        .clone().ok_or_else(||anyhow!("cut snapshot was not initialized"))?;
    let mut snap:Value=serde_json::from_slice(&B64.decode(encoded)?)?;
    let cut=&m["cut"];
    let index=cut["index"].as_u64().ok_or_else(||anyhow!("invalid cut index"))?;
    let start=cut["start"].as_f64().ok_or_else(||anyhow!("invalid cut start"))?;
    let end=cut["end"].as_f64().ok_or_else(||anyhow!("invalid cut end"))?;
    let duration=cut["duration"].as_f64().ok_or_else(||anyhow!("invalid cut duration"))?;
    ensure!(start>=0.&&end>start&&duration>0.&&duration<86400.,"invalid cut timing");
    let target_id=TARGET.load(Ordering::SeqCst);
    let edits=cut["edits"].clone();
    let audio_ref=if first {save_audio_ref(&m["audioData"])?}else{None};
    let result=EDIT.call_edit_section(move |e|->AnyResult<()> {
        let fps=*e.info.fps.numer() as f64 / *e.info.fps.denom() as f64;
        ensure!(fps>0.,"invalid scene frame rate");
        let origin=if first {
            let mut source_origin=None;
            let mut cut_origin=None;
            for layer in 0..=e.info.layer_max {
                let mut frame=0;
                while let Some(ob)=e.find_object_after(layer,frame)? {
                    let lf=e.get_object_layer_frame(ob)?;
                    if target_id!=0 && e.count_object_effect(ob,"JIZURA")?>0 {
                        let encoded=e.get_object_effect_item(ob,"JIZURA",0,"編集データ")?;
                        if let Ok(bytes)=B64.decode(encoded) {
                            if let Ok(existing)=serde_json::from_slice::<Value>(&bytes) {
                                if e.get_object_id(ob)?==target_id && existing["nativeCut"].as_u64().is_none() {
                                    source_origin=Some(lf.start);
                                    *NATIVE_OLD_SOURCE.lock().map_err(|_|anyhow!("old source lock poisoned"))?=Some(target_id);
                                }
                                if existing["nativeOwner"].as_i64()==Some(target_id) && existing["nativeCut"].as_u64().is_some() {
                                    let cut_start=existing["nativeCutStart"].as_f64().unwrap_or(0.);
                                    let candidate=lf.start.saturating_sub((cut_start*fps).round() as usize);
                                    cut_origin=Some(cut_origin.map_or(candidate,|v:usize|v.min(candidate)));
                                }
                            }
                        }
                    }
                    if lf.end<frame {break;}frame=lf.end.saturating_add(1);
                }
            }
            let origin=source_origin.or(cut_origin).unwrap_or(e.info.frame);
            *NATIVE_CUT_ORIGIN.lock().map_err(|_|anyhow!("cut origin lock poisoned"))?=Some(origin);
            origin
        } else {NATIVE_CUT_ORIGIN.lock().map_err(|_|anyhow!("cut origin lock poisoned"))?.ok_or_else(||anyhow!("cut origin missing"))?};
        let object_start=origin.saturating_add((start*fps).round() as usize);
        let object_end=origin.saturating_add((end*fps).round() as usize);
        let object_len=object_end.saturating_sub(object_start).max(1);
        if first {
            for layer in 0..=e.info.layer_max {
                let mut frame=0;
                while let Some(ob)=e.find_object_after(layer,frame)? {
                    let lf=e.get_object_layer_frame(ob)?;
                    if e.count_object_effect(ob,"JIZURA")?>0 {
                        let encoded=e.get_object_effect_item(ob,"JIZURA",0,"編集データ")?;
                        if let Ok(bytes)=B64.decode(encoded) {
                            if let Ok(snap)=serde_json::from_slice::<Value>(&bytes) {
                                if snap["nativeOwner"].as_i64()==Some(target_id) && snap["nativeCut"].as_u64().is_some() {
                                    e.delete_object(ob)?;
                                }
                            }
                        }
                    }
                    if lf.end<frame {break;}
                    frame=lf.end.saturating_add(1);
                }
            }
            let mut free=NATIVE_CUT_FREE_LAYERS.lock().map_err(|_|anyhow!("cut layer pool lock poisoned"))?;
            free.clear();
            for layer in 0..=e.info.layer_max {
                if e.get_layer_name(layer)?.unwrap_or_default().starts_with("JZ Cut")
                    && e.find_object_after(layer,0)?.is_none() { free.push_back(layer); }
            }
            let shared=free.pop_front().unwrap_or_else(||e.info.layer_max.saturating_add(1));
            for old in free.drain(..) { e.set_layer_name(old,None)?; }
            free.push_back(shared);
        }
        let layer=*NATIVE_CUT_FREE_LAYERS.lock().map_err(|_|anyhow!("cut layer pool lock poisoned"))?
            .front().ok_or_else(||anyhow!("カットレイヤーを確保できませんでした"))?;
        native_trace(&format!("cut create: {} layer={layer}",index+1));
        let ob=e.create_object("JIZURA",layer,object_start,Some(object_len))?;
        // Disable before assigning the expensive snapshot so the editor does
        // not ask the renderer to draw while this edit section is open.
        if first {e.set_layer_enable(layer,false)?;}
        native_trace("cut disabled");
        let owner=if first {e.get_object_id(ob)?}else{target_id};
        snap["nativeCut"]=Value::from(index);
        snap["nativeCutStart"]=Value::from(start);
        snap["nativeCutDuration"]=Value::from(duration);
        snap["nativeOwner"]=Value::from(owner);
        if let Some(name)=audio_ref {snap["audioRef"]=Value::String(name);}
        if let Some((rate,channels,bytes))=NATIVE_CUT_PCM.lock().map_err(|_|anyhow!("cut audio lock poisoned"))?.as_ref() {
            let sample_start=(((object_start-origin) as f64/fps)* *rate as f64).round() as usize;
            let sample_end=(((object_end-origin) as f64/fps)* *rate as f64).round() as usize;
            let lo=sample_start.saturating_mul(*channels).saturating_mul(4).min(bytes.len());
            let hi=sample_end.saturating_mul(*channels).saturating_mul(4).min(bytes.len()).max(lo);
            let mut samples=Vec::with_capacity((hi-lo)/2);
            for b in bytes[lo..hi].chunks_exact(4) {
                let value=f32::from_le_bytes(b.try_into().unwrap());
                let sample=(value.clamp(-1.,1.)*32767.).round() as i16;
                samples.extend_from_slice(&sample.to_le_bytes());
            }
            snap["pcm"]=serde_json::json!({"rate":rate,"channels":channels,"format":"s16","data":B64.encode(samples)});
        }
        native_trace("cut set snapshot begin");
        e.set_object_effect_item(ob,"JIZURA",0,"編集データ",&B64.encode(serde_json::to_vec(&snap)?))?;
        native_trace("cut set snapshot end");
        if first {TARGET.store(owner,Ordering::SeqCst);}
        if let Some(text)=edits["text"].as_str(){e.set_object_effect_item(ob,"JIZURA",0,"カット文字",text)?;}
        for (name,group) in CUT_SELECT_ITEMS {e.set_object_effect_item(ob,"JIZURA",0,name,&edit_choice_value(&edits,name,group).to_string())?;}
        native_trace("cut selects end");
        for (name,role) in FONT_ITEMS {native_trace(&format!("cut font {name}"));e.set_object_effect_item(ob,"JIZURA",0,name,&font_index(edits["fonts"][role].as_str().unwrap_or("")).to_string())?;}
        for (name,key) in FX_ITEMS {if let Some(value)=edits["fx"][key].as_f64(){native_trace(&format!("cut fx {name}"));e.set_object_effect_item(ob,"JIZURA",0,name,&value.clamp(0.,1.).to_string())?;}}
        for (name,key) in COLOR_ITEMS {if let Some(value)=edits["colors"][key].as_str(){native_trace(&format!("cut color {name}"));e.set_object_effect_item(ob,"JIZURA",0,name,&format!("#{:06X}",color_code(value)))?;}}
        // AviUtl2 aborts when set_object_effect_item writes these Check controls.
        // Their current values live in the cut snapshot until the user edits them.
        native_trace("cut bpm");
        e.set_object_effect_item(ob,"JIZURA",0,"共通BPM（適用で反映）",&edits["bpm"].as_f64().unwrap_or(0.).clamp(0.,300.).to_string())?;
        native_trace("cut snapshot set");
        let name=edits["text"].as_str().unwrap_or("").split_whitespace().collect::<Vec<_>>().join(" ");
        let name=if name.is_empty(){format!("カット {:03}",index+1)}else{name};
        e.set_object_name(ob,Some(&name))?;
        if first {e.set_layer_name(layer,Some("JZ Cuts"))?;}
        Ok(())
    });
    result??;
    native_trace(&format!("cut done: {}/{}",m["done"].as_u64().unwrap_or(0),m["total"].as_u64().unwrap_or(0)));
    if m["final"].as_bool().unwrap_or(false) {
        *NATIVE_CUT_SNAPSHOT.lock().map_err(|_|anyhow!("cut snapshot lock poisoned"))?=None;
        *NATIVE_CUT_PCM.lock().map_err(|_|anyhow!("cut audio lock poisoned"))?=None;
        *NATIVE_CUT_ORIGIN.lock().map_err(|_|anyhow!("cut origin lock poisoned"))?=None;
    }
    native_ack(m["done"].as_u64().unwrap_or(0),m["total"].as_u64().unwrap_or(0),None);
    Ok(())
}
fn apply_mode(m:&Value)->AnyResult<()> {
    let stage=m["stage"].as_str().ok_or_else(||anyhow!("missing mode stage"))?.to_owned();
    ensure!(stage=="source","invalid mode stage");
    let enabled=m["enabled"].as_bool().ok_or_else(||anyhow!("missing mode value"))?;
    let target_id=TARGET.load(Ordering::SeqCst);
    ensure!(target_id!=0,"先に『適用』で映像オブジェクトを作成してください");
    native_trace(&format!("mode start {stage} {enabled}"));
    let result=EDIT.call_edit_section(move |e|->AnyResult<()> {
        let mut cut_layers=Vec::new();
        for layer in 0..=e.info.layer_max {
            let mut frame=0;
            while let Some(ob)=e.find_object_after(layer,frame)? {
                if e.count_object_effect(ob,"JIZURA")?>0 {
                    let encoded=e.get_object_effect_item(ob,"JIZURA",0,"編集データ")?;
                    if let Ok(bytes)=B64.decode(encoded) {
                        if let Ok(snap)=serde_json::from_slice::<Value>(&bytes) {
                            if snap["nativeOwner"].as_i64()==Some(target_id) && snap["nativeCut"].as_u64().is_some() {
                                cut_layers.push(layer);
                                break;
                            }
                        }
                    }
                }
                let lf=e.get_object_layer_frame(ob)?;
                if lf.end<frame {break;}frame=lf.end.saturating_add(1);
            }
        }
        if enabled {ensure!(!cut_layers.is_empty(),"カットが見つかりません。『適用』で再生成してください");}
        for &layer in &cut_layers {e.set_layer_enable(layer,enabled)?;}
        if enabled {
            if let Some(old_id)=*NATIVE_OLD_SOURCE.lock().map_err(|_|anyhow!("old source lock poisoned"))? {
                'old_source: for layer in 0..=e.info.layer_max {
                    let mut frame=0;
                    while let Some(ob)=e.find_object_after(layer,frame)? {
                        if e.get_object_id(ob)?==old_id {e.delete_object(ob)?;break 'old_source;}
                        let lf=e.get_object_layer_frame(ob)?;
                        if lf.end<frame {break;}frame=lf.end.saturating_add(1);
                    }
                }
            }
        }
        Ok(())
    });
    result??;
    if enabled {*NATIVE_OLD_SOURCE.lock().map_err(|_|anyhow!("old source lock poisoned"))?=None;}
    let done=m["done"].as_u64().unwrap_or(0);
    let total=m["total"].as_u64().unwrap_or(0);
    native_trace(&format!("mode done: {done}/{total}"));
    native_ack(done,total,None);
    Ok(())
}
aviutl2::register_generic_plugin!(JizuraPlugin);

/// Generate the native filter dropdown catalog from the vendored browser engine.
pub fn export_catalog(base:&std::path::Path)->AnyResult<()> {
    let dist=base.join("dist");
    let wide:Vec<u16>=dist.to_string_lossy().encode_utf16().chain(Some(0)).collect();
    unsafe{jz_init(wide.as_ptr());}
    let request=B64.encode(r#"{"version":1,"catalogOnly":true}"#);
    let frame=unsafe{jz_render(c(&request).as_ptr(),0.,false)};
    if frame.is_null(){bail!("Catalog export failed: {}",unsafe{CStr::from_ptr(jz_error())}.to_string_lossy());}
    unsafe{jz_free_frame(frame);}
    let data=std::fs::read(dist.join("audit-catalog.json"))?;
    let catalog:Value=serde_json::from_slice(&data)?;
    ensure!(catalog["group"]=="catalog","Unexpected catalog response");
    let target=base.join("catalog.json");
    std::fs::write(&target,serde_json::to_vec_pretty(&catalog)?)?;
    println!("Updated {}",target.display());
    Ok(())
}

/// Standalone integration check, using the same WebView renderer as the plugin.
pub fn smoke_test(base:&std::path::Path)->AnyResult<()> {
    let wide:Vec<u16>=base.to_string_lossy().encode_utf16().chain(Some(0)).collect();unsafe{jz_init(wide.as_ptr());}
    let result=(||->AnyResult<()>{
        let catalog=B64.encode(r#"{"version":1,"catalogOnly":true}"#);
        let frame=unsafe{jz_render(c(&catalog).as_ptr(),0.,false)};
        if frame.is_null(){bail!("Catalog export failed: {}",unsafe{CStr::from_ptr(jz_error())}.to_string_lossy());}
        unsafe{jz_free_frame(frame);}
        // Warm up WebView2 and the shared frame buffer before comparing frames.
        let warmup=unsafe{jz_render(c(DEFAULT).as_ptr(),0.,false)};
        if warmup.is_null(){bail!("Renderer warmup failed: {}",unsafe{CStr::from_ptr(jz_error())}.to_string_lossy());}
        unsafe{jz_free_frame(warmup);}
        let mut hashes=Vec::new();
        for (i,t) in [1.0,2.0,4.0,1.0].into_iter().enumerate(){
            let frame=unsafe{jz_render(c(DEFAULT).as_ptr(),t,false)};
            if frame.is_null(){bail!("{}",unsafe{CStr::from_ptr(jz_error())}.to_string_lossy());}
            let(mut w,mut h)=(0,0);let p=unsafe{jz_pixels(frame,&mut w,&mut h)};
            let data=unsafe{std::slice::from_raw_parts(p,w as usize*h as usize*4)};
            ensure!(w==1920&&h==1080,"Unexpected sample resolution");
            let hash=data.iter().fold(0xcbf29ce484222325u64,|a,b|(a^*b as u64).wrapping_mul(0x100000001b3));hashes.push(hash);
            // Save an uncompressed 32-bit BMP for visual inspection.
            let mut bmp=Vec::new();bmp.extend_from_slice(b"BM");bmp.extend_from_slice(&(54u32+data.len() as u32).to_le_bytes());bmp.extend_from_slice(&[0;4]);bmp.extend_from_slice(&54u32.to_le_bytes());bmp.extend_from_slice(&40u32.to_le_bytes());bmp.extend_from_slice(&w.to_le_bytes());bmp.extend_from_slice(&(-h).to_le_bytes());bmp.extend_from_slice(&1u16.to_le_bytes());bmp.extend_from_slice(&32u16.to_le_bytes());bmp.extend_from_slice(&[0;24]);
            for px in data.chunks_exact(4){bmp.extend_from_slice(&[px[2],px[1],px[0],px[3]]);}
            std::fs::write(base.join(format!("smoke-{i}.bmp")),bmp)?;unsafe{jz_free_frame(frame);}
            println!("frame {t}s: {w}x{h}, hash {hash:x}");
        }
        ensure!(hashes[0]!=hashes[1],"Animation did not advance");
        ensure!(hashes[0]==hashes[3],"Random-access rendering is not deterministic");
        let cut_snapshot=B64.encode(r#"{"version":1,"nativeCut":0,"nativeCutDuration":2.0}"#);
        let cut=unsafe{jz_render(c(&cut_snapshot).as_ptr(),0.2,false)};
        if cut.is_null(){bail!("Cut rendering failed: {}",unsafe{CStr::from_ptr(jz_error())}.to_string_lossy());}
        let(mut cw,mut ch)=(0,0);let pixels=unsafe{jz_pixels(cut,&mut cw,&mut ch)};
        ensure!(cw==1920&&ch==1080&&!pixels.is_null(),"Invalid cut frame");
        let original_hash=unsafe{std::slice::from_raw_parts(pixels,cw as usize*ch as usize*4)}.iter().fold(0xcbf29ce484222325u64,|a,b|(a^*b as u64).wrapping_mul(0x100000001b3));
        unsafe{jz_free_frame(cut);}
        println!("native cut: {cw}x{ch}");
        let edited=B64.encode(r#"{"version":1,"nativeCut":0,"nativeCutDuration":2.0,"nativeEdit":{"text":"編集後の文字","enter":"グリント","decor":"なし"}}"#);
        let frame=unsafe{jz_render(c(&edited).as_ptr(),0.2,false)};
        if frame.is_null(){bail!("Native cut edit failed: {}",unsafe{CStr::from_ptr(jz_error())}.to_string_lossy());}
        let(mut ew,mut eh)=(0,0);let pixels=unsafe{jz_pixels(frame,&mut ew,&mut eh)};
        ensure!(ew==1920&&eh==1080&&!pixels.is_null(),"Invalid edited cut frame");
        let edited_hash=unsafe{std::slice::from_raw_parts(pixels,ew as usize*eh as usize*4)}.iter().fold(0xcbf29ce484222325u64,|a,b|(a^*b as u64).wrapping_mul(0x100000001b3));
        unsafe{jz_free_frame(frame);}
        ensure!(edited_hash!=original_hash,"Native cut edits did not change the frame");
        println!("native edit: frame changed ({edited_hash:x})");
        let mut timings=Vec::new();
        for i in 0..16 {
            let start=std::time::Instant::now();
            let frame=unsafe{jz_render(c(&cut_snapshot).as_ptr(),0.15+i as f64/24.0,false)};
            if frame.is_null(){bail!("Cut benchmark failed: {}",unsafe{CStr::from_ptr(jz_error())}.to_string_lossy());}
            timings.push(start.elapsed().as_secs_f64()*1000.0);
            unsafe{jz_free_frame(frame);}
        }
        let steady=&timings[2..];
        println!("cut benchmark: {:.1} ms/frame (min {:.1}, max {:.1})",steady.iter().sum::<f64>()/steady.len() as f64,steady.iter().copied().fold(f64::INFINITY,f64::min),steady.iter().copied().fold(0.0,f64::max));
        for (label,snapshot,transparent) in [
            ("audio carrier",r#"{"version":1,"audioOnly":true}"#,false),
            ("HUD overlay",r#"{"version":1,"nativeHud":true}"#,true),
        ] {
            let frame=unsafe{jz_render(c(&B64.encode(snapshot)).as_ptr(),1.0,transparent)};
            if frame.is_null(){bail!("{label} rendering failed: {}",unsafe{CStr::from_ptr(jz_error())}.to_string_lossy());}
            let(mut w,mut h)=(0,0);let pixels=unsafe{jz_pixels(frame,&mut w,&mut h)};
            ensure!(w==1920&&h==1080&&!pixels.is_null(),"Invalid {label} frame");
            let data=unsafe{std::slice::from_raw_parts(pixels,w as usize*h as usize*4)};
            if label=="audio carrier" {ensure!(data.chunks_exact(4).all(|p|p[3]==0),"Audio carrier is not transparent");}
            if label=="HUD overlay" {ensure!(data.chunks_exact(4).any(|p|p[3]>0),"HUD overlay is empty");}
            unsafe{jz_free_frame(frame);}
            println!("{label}: {w}x{h}");
        }
        let large=B64.encode(r#"{"version":1,"audioOnly":true,"project":{"aspect":"16:9","res":1440}}"#);
        let frame=unsafe{jz_render(c(&large).as_ptr(),0.,false)};
        if frame.is_null(){bail!("Shared buffer resize failed: {}",unsafe{CStr::from_ptr(jz_error())}.to_string_lossy());}
        let(mut w,mut h)=(0,0);let pixels=unsafe{jz_pixels(frame,&mut w,&mut h)};
        ensure!(w==2560&&h==1440&&!pixels.is_null(),"Invalid resized shared frame");
        unsafe{jz_free_frame(frame);}
        println!("shared buffer resize: {w}x{h}");
        Ok(())
    })();unsafe{jz_shutdown();}result
}

pub fn audit_test(base:&std::path::Path)->AnyResult<()> {
    let wide:Vec<u16>=base.to_string_lossy().encode_utf16().chain(Some(0)).collect();unsafe{jz_init(wide.as_ptr());}
    let result=(||->AnyResult<()>{
        for group in ["layout","enter","hold","exit","decor","treat","bg","cam","fx","trans","styles"]{
            let s=B64.encode(format!("{{\"auditGroup\":\"{group}\"}}"));let frame=unsafe{jz_render(c(&s).as_ptr(),1.,false)};
            if frame.is_null(){bail!("{}: {}",group,unsafe{CStr::from_ptr(jz_error())}.to_string_lossy());}unsafe{jz_free_frame(frame);}
            let report:Value=serde_json::from_slice(&std::fs::read(base.join(format!("audit-{group}.json")))?)?;
            println!("{}: {} expressions, {} failures",group,report["count"],report["failed"]);
        }Ok(())
    })();unsafe{jz_shutdown();}result
}
