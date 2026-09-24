/* AviUtl2 integration. The original JIZURA engine and editor remain intact. */
(() => {
  'use strict';
  const host = window.chrome && chrome.webview;
  const send = x => host && host.postMessage(JSON.stringify(x));
  const enc = s => bytes64(new TextEncoder().encode(s));
  function bytes64(a) { let s=''; for(let i=0;i<a.length;i+=32768) s+=String.fromCharCode(...a.subarray(i,i+32768)); return btoa(s); }
  const dec = s => new TextDecoder().decode(Uint8Array.from(atob(s),c=>c.charCodeAt(0)));
  let audioData = null;
  let fontFiles = [];
  let nativeAckResolve=null,nativeAckReject=null,cutMode=false;
  let commonBpmResolve=null;
  window.aviCommonBpmAck=value=>{const resolve=commonBpmResolve;commonBpmResolve=null;if(resolve)resolve(value);};
  let generating=false;
  function setGenerating(value) {
    generating=value;
    for(const id of ['avi-apply','avi-native']) {
      const button=document.getElementById(id);
      if(button)button.disabled=value;
    }
  }
  window.aviNativeAck=(done,total,error)=>{
    const resolve=nativeAckResolve,reject=nativeAckReject;nativeAckResolve=nativeAckReject=null;
    if(error){if(reject)reject(Error(error));return;}
    if(resolve)resolve({done,total});
  };
  const fontLoader=J.loadFontFile;
  J.loadFontFile=async f=>{const data=bytes64(new Uint8Array(await f.arrayBuffer()));const key=await fontLoader(f);fontFiles=fontFiles.filter(x=>x.name!==f.name);fontFiles.push({name:f.name,data});return key;};
  async function restoreFonts(files){for(const f of files||[])await fontLoader(new File([Uint8Array.from(atob(f.data),c=>c.charCodeAt(0))],f.name));}
  const analyze = J.analyzeAudio;
  J.analyzeAudio = async f => { audioData={name:f.name,data:bytes64(new Uint8Array(await f.arrayBuffer()))}; return analyze(f); };
  function audioInfo(a) {
    if (!a) return null;
    return {name:a.name,duration:a.duration,sampleRate:a.sampleRate,bpm:a.bpm,
      beats:Array.from(a.beats||[]),energy:Array.from(a.energy||[]),energyRate:a.energyRate,peaks:Array.from(a.peaks||[])};
  }
  function planningAudio(p,a) {
    if (p.timing.bpm>0) return Object.assign({},a||{}, {beats:J.beatGrid(p.timing.bpm,p.timing.beatOffset||0,(a&&a.duration)||600)});
    return a;
  }
  let currentKey='', plan=null, planBase=null, renderer=new J.Renderer();
  const canvas=document.createElement('canvas');
  const ctx=canvas.getContext('2d',{willReadFrequently:true});
  let frameBuffer=null,pendingBuffer=null;
  if(host)host.addEventListener('sharedbufferreceived',event=>{
    if(event.additionalData&&event.additionalData.type==='framebuffer') {
      if(frameBuffer)try{host.releaseBuffer(frameBuffer);}catch(_){}
      frameBuffer=event.getBuffer();
      send({type:'bufferReady'});
      if(pendingBuffer) {pendingBuffer.resolve();pendingBuffer=null;}
    }
  });
  async function sendFrameRaw(id,report,frameStart) {
    const drawMs=performance.now()-frameStart;
    const w=canvas.width,h=canvas.height,byteLength=w*h*4;
    if(!frameBuffer||frameBuffer.byteLength<byteLength) {
      if(pendingBuffer)throw Error('Frame buffer resize is already in progress');
      await new Promise((resolve,reject)=>{
        const timer=setTimeout(()=>{pendingBuffer=null;reject(Error('Shared frame buffer timed out'));},5000);
        pendingBuffer={resolve:()=>{clearTimeout(timer);resolve();}};
        send({type:'bufferNeeded',id,bytes:byteLength});
      });
    }
    if(!frameBuffer||frameBuffer.byteLength<byteLength)throw Error('Shared frame buffer is too small');
    new Uint8Array(frameBuffer,0,byteLength).set(ctx.getImageData(0,0,w,h).data);
    send({type:'frameRaw',id,w,h,report,profile:{drawMs,transferMs:performance.now()-frameStart-drawMs}});
  }
  async function audit(group) {
    const keys=group==='styles'?J.STYLE_ORDER:J.order(group), results=[];
    const cv=document.createElement('canvas');cv.width=320;cv.height=180;const cx=cv.getContext('2d',{willReadFrequently:true});
    for(const key of keys){
      const errors=[],warn=console.warn;console.warn=(...a)=>errors.push(a.map(x=>String(x&&x.stack||x)).join(' '));
      try{
        J.aviResetGlyphs();const p=J.defaultProject();p.extra=true;p.lyrics='字面の世界\n光が踊る';p.fx.density=0;p.fx.glitch=0;p.fx.chroma=0.7;p.fx.decor=1;p.timing.offset=0;
        const ov={single:true,layout:'center',enter:'cut',exit:'cut',hold:'still',decor:[],treat:'none',bg:'none',cam:'push'};
        if(group==='styles')p.style=key;else if(group==='decor')ov.decor=[key];else if(group!=='fx')ov[group]=key;
        p.overrides={0:{...ov},1:{...ov}};
        const pl=J.plan(p,null),cs=pl.cuts.filter(x=>x.line>=0);let cut=group==='trans'?cs[1]:cs[0];
        if(!cut)throw Error('no cut');
        let times=[cut.start+Math.min(cut.dur*.15,.12),cut.start+cut.dur*.55,cut.end-.08];
        if(group==='fx'){pl.events=[{t:cut.start,type:key,amp:1,dur:1}];times=[cut.start+.12,cut.start+.5];}
        if(group==='trans')times=[cut.start+.03,cut.start+(cut.transDur||.3)*.55];
        const random=Math.random;Math.random=J.rng(p.seed);
        try{const r=new J.Renderer();for(const t of times){cv.width=320;r.frame(cx,pl,t,{scale:320/pl.W});}}finally{Math.random=random;}
        results.push({key,ok:errors.length===0,errors});
      }catch(e){results.push({key,ok:false,errors:[...errors,String(e.stack||e)]});}finally{console.warn=warn;}
      // Yield to keep the worker message queue responsive between expressions.
      await new Promise(r=>setTimeout(r,0));
    }
    return {group,count:keys.length,failed:results.filter(x=>!x.ok).length,results};
  }
  window.aviRender=async (id,encoded,t,transparent) => {
    try {
      const frameStart=performance.now();
      const snap=JSON.parse(dec(encoded));
      let report=snap.auditGroup?await audit(snap.auditGroup):null;
      if(snap.catalogOnly){
        const groups={};
        for(const group of ['layout','enter','hold','exit','decor','treat','bg','cam','trans'])
          groups[group]=J.order(group).filter(key=>J.registry(group)[key]).map(key=>({key,name:J.registry(group)[key].name}));
        report={group:'catalog',groups};canvas.width=2;canvas.height=2;ctx.clearRect(0,0,2,2);
        await sendFrameRaw(id,report,frameStart);return;
      }
      const p=snap.project||J.defaultProject();
      if(Number.isInteger(snap.nativeCut)&&snap.nativeEdit){
        const edit=snap.nativeEdit;
        if(edit.fx){const {density,...visualFx}=edit.fx;p.fx={...p.fx,...visualFx};}
        if(edit.colors)p.colors={...p.colors,...edit.colors};
        if(edit.fonts)p.fonts={...p.fonts,...edit.fonts};
      }
      if(snap.audioOnly) {
        const [w,h]=J.outputSize(p);
        canvas.width=w;canvas.height=h;
        ctx.clearRect(0,0,w,h);
        await sendFrameRaw(id,report,frameStart);
        return;
      }
      const cacheKey=(Number.isInteger(snap.nativeCut)||snap.nativeHud)?JSON.stringify({project:p,audio:snap.audio,fontFiles:snap.fontFiles}):encoded;
      if(currentKey!==cacheKey) {
        await restoreFonts(snap.fontFiles);
        for(const f of p.userFonts||[]) if(!J.FONTS[f.key]) J.addUserFont(f.key,f.label,f.family,f.weight||400);
        planBase=J.plan(p,planningAudio(p,snap.audio));
        plan=planBase;
        await J.ensureFonts(p.lyrics+(p.title||'')+(p.artist||'')+'0123456789:./-_()No.LYRICRECUNTITLED',J.fontsOfPlan(plan));
        renderer=new J.Renderer();
        const [w,h]=J.outputSize(p); canvas.width=w;canvas.height=h;
        J.glyphs.maxRes=h>=1000?768:512;
        currentKey=cacheKey;
      }
      // Upstream keeps mutable glyph IDs, texture RNG and recipe caches. A host
      // can request frames in any order, so reset frame-local state deterministically.
      J.aviResetGlyphs();
      // reset() clears the frame and drawing state without reallocating the canvas.
      if(ctx.reset)ctx.reset();else canvas.width=canvas.width;
      const fullPlan=(Number.isInteger(snap.nativeCut)||snap.nativeHud)?planBase:J.plan(p,planningAudio(p,snap.audio));
      let renderOptions={scale:canvas.width/fullPlan.W,transparent:!!transparent};
      if(snap.nativeHud) {
        const scale=canvas.width/fullPlan.W,stepDur=J.stepDur(fullPlan.fx,fullPlan.fps),clock=J.komaOf(fullPlan.fx)>0?stepDur:1/24;
        const tq=Math.floor(t/stepDur+1e-6)*stepDur,cut=J.cutAt(fullPlan,tq);
        const sc=fullPlan.style.schemes[cut?cut.scheme%fullPlan.style.schemes.length:0]||fullPlan.style.schemes[0];
        const step=Math.floor(tq/clock+1e-6);
        ctx.setTransform(1,0,0,1,0,0);ctx.clearRect(0,0,canvas.width,canvas.height);ctx.setTransform(scale,0,0,scale,0,0);
        const env=renderer.makeEnv(ctx,fullPlan,cut,sc,{pass:'main',t:tq,lt:cut?tq-cut.start:0,ltb:cut?tq-cut.start:0,step,scale,allowFilter:true,energy:null,beat:null});
        J.drawHUD(env,fullPlan);
      } else if(Number.isInteger(snap.nativeCut)) {
        const sourcePos=fullPlan.cuts.findIndex(c=>c.index===snap.nativeCut),originalCut=fullPlan.cuts[sourcePos];
        if(!originalCut)throw Error('native cut index not found: '+snap.nativeCut);
        const edit=snap.nativeEdit||{},sourceCut={...originalCut};
        const resolve=(group,value,fallback)=>{
          if(!value)return fallback;
          const registry=J.registry(group);
          if(registry[value])return value;
          return J.order(group).find(key=>registry[key]&&registry[key].name===value)||fallback;
        };
        if(typeof edit.text==='string'&&edit.text){sourceCut.text=edit.text;sourceCut.lineText=edit.text;sourceCut.words=J.chunkText(edit.text);}
        for(const group of ['layout','enter','hold','exit','treat','bg','cam','trans']){
          if(!edit[group])continue;
          const old=sourceCut[group],key=group==='trans'&&edit[group]==='なし'?null:resolve(group,edit[group],old);
          sourceCut[group]=key;
          if(key&&key!==old){
            const def=J.registry(group)[key],rng=J.rng(sourceCut.seed);
            if(group==='layout'&&def.plan)sourceCut.params=def.plan(rng,{text:sourceCut.text,n:J.glyphCount(sourceCut.text),W:fullPlan.W,H:fullPlan.H,dur:sourceCut.dur},fullPlan.style);
            if(['treat','bg','cam','trans'].includes(group))sourceCut[group+'P']=def.plan?def.plan(rng,fullPlan.style):{};
          }
        }
        if(typeof edit.decor==='string'){
          const names=edit.decor.split(/[、,\n]/).map(x=>x.trim()).filter(Boolean);
          sourceCut.decor=names.map((name,i)=>{
            const id=resolve('decor',name,null);if(!id)return null;
            return {...(originalCut.decor[i]||originalCut.decor[0]||{}),id,seed:(sourceCut.seed+i+1)|0};
          }).filter(Boolean);
        }
        const prev=sourcePos>0?fullPlan.cuts[sourcePos-1]:null;
        const withTransition=!!(sourceCut.trans&&prev&&Math.abs(prev.end-sourceCut.start)<0.06);
        const origin=withTransition?prev.start:sourceCut.start, originalDur=sourceCut.end-sourceCut.start;
        const duration=Math.max(originalDur,Number(snap.nativeCutDuration)||originalDur);
        const prevDur=withTransition?prev.end-prev.start:0;
        const previous=withTransition?{...prev,start:-prevDur,end:0,dur:prevDur,index:0,trans:null,transP:null,transDur:0}:null;
        const cut={...sourceCut,start:0,end:duration,dur:originalDur,index:withTransition?1:0};
        const events=fullPlan.events.filter(e=>e.t>=origin-0.8&&e.t<=origin+duration).map(e=>({...e,t:e.t-origin}));
        const beatStart=Math.max(0,Math.floor(origin*(fullPlan.energyRate||1)));
        const beatEnd=Math.ceil((origin+duration)*(fullPlan.energyRate||1));
        plan={...fullPlan,duration,cuts:previous?[previous,cut]:[cut],events,beats:(fullPlan.beats||[]).filter(b=>b>=origin&&b<=origin+duration).map(b=>b-origin),
          energy:fullPlan.energy?fullPlan.energy.slice(beatStart,beatEnd):fullPlan.energy,hud:false};
        renderOptions={...renderOptions,noHud:true};
      } else plan=fullPlan;
      const random=Math.random;
      // Keep the renderer's scratch canvases and caches across frames. Creating
      // one here for every native layer/frame rapidly grows WebView memory use.
      try { Math.random=J.rng(p.seed);
        if(!snap.nativeHud)renderer.frame(ctx,plan,t,renderOptions);
        if(Number.isInteger(snap.nativeCut)) {
          const sourceCut=fullPlan.cuts.find(c=>c.index===snap.nativeCut);
          const scale=canvas.width/fullPlan.W,stepDur=J.stepDur(fullPlan.fx,fullPlan.fps),clock=J.komaOf(fullPlan.fx)>0?stepDur:1/24;
          const tq=Math.floor((sourceCut.start+t)/stepDur+1e-6)*stepDur,cut=J.cutAt(fullPlan,tq);
          const sc=fullPlan.style.schemes[cut?cut.scheme%fullPlan.style.schemes.length:0]||fullPlan.style.schemes[0];
          const step=Math.floor(tq/clock+1e-6);
          ctx.setTransform(scale,0,0,scale,0,0);
          const env=renderer.makeEnv(ctx,fullPlan,cut,sc,{pass:'main',t:tq,lt:cut?tq-cut.start:0,ltb:cut?tq-cut.start:0,step,scale,allowFilter:true,energy:null,beat:null});
          J.drawHUD(env,fullPlan);
        }
      } finally {Math.random=random;}
      await sendFrameRaw(id,report,frameStart);
    } catch(e) { send({type:'error',id,error:String(e.stack||e)}); }
  };
  window.aviLoad=async encoded => {
    const snap=JSON.parse(dec(encoded));
    if (!J.aviSetProject) return;
    cutMode=!!snap.nativeHud;
    audioData=snap.audioData||null;
    fontFiles=snap.fontFiles||[]; await restoreFonts(fontFiles);
    let a=null;
    if(audioData) a=await analyze(new File([Uint8Array.from(atob(audioData.data),c=>c.charCodeAt(0))],audioData.name));
    J.aviSetProject(snap.project||J.defaultProject(),a);
    document.getElementById('avi-status').textContent='編集中のオブジェクトに「適用」で反映します';
  };
  async function snapshot(sample=false,native=false) {
    if(generating)return;
    if(sample) { audioData=null; fontFiles=[]; J.aviSetProject(J.defaultProject(),null); }
    const p=JSON.parse(JSON.stringify(J.ui.project));
    const a=audioInfo(J.ui.audio);
    const pl=J.plan(p,planningAudio(p,a));
    const s={version:1,project:p,audio:a,audioData,fontFiles,nativeHud:cutMode&&!sample};
    let pcm=null;
    if(!native && J.ui.audio && p.includeAudio!==false) {
      const b=J.ui.audio.buffer, channels=Math.min(2,b.numberOfChannels);
      const f=new Float32Array(b.length*channels);
      for(let c=0;c<channels;c++){const d=b.getChannelData(c);for(let i=0;i<b.length;i++)f[i*channels+c]=d[i];}
      pcm={rate:b.sampleRate,channels,data:bytes64(new Uint8Array(f.buffer))};
    }
    const [w,h]=J.outputSize(p);
    const message={type:sample?'sample':native?'native':'apply',snapshot:native?null:enc(JSON.stringify(s)),duration:pl.duration,w,h,fps:pl.fps,pcm};
    if(native) {
      const segmenter=window.Intl&&Intl.Segmenter?new Intl.Segmenter('ja',{granularity:'grapheme'}):null;
      message.hud=false;
      message.cuts=(pl.cuts||[]).filter(c=>c.end>c.start&&String(c.text||'').length>0).map(c=>{
        const chars=segmenter?[...segmenter.segment(c.text||'')].map(x=>x.segment):[...(c.text||'')];
        const fk=(c.params&&c.params.font)||((pl.style.fonts.display||[])[0])||'gothic_bold';
        const family=/mincho|tokumin|shippori|klee|brush/.test(fk)?'Yu Mincho':'Yu Gothic';
        const sc=pl.style.schemes[(c.scheme||0)%pl.style.schemes.length]||pl.style.schemes[0];
        const next=(pl.cuts||[]).find(n=>n.index===c.index+1);
        const transTail=next&&next.trans&&Math.abs(c.end-next.start)<0.06?(next.transDur||0.35):0;
        return {index:c.index,line:c.line,start:c.start,end:c.end,nativeDuration:c.end-c.start+transTail,text:c.text||'',layout:c.layout,font:family,
          color:String(sc.fg||'#ffffff').replace('#',''),advances:chars.map(ch=>Math.max(.25,J.metrics.adv(fk,ch))),chars};
      });
      const chunks=[];let batch=[],batchSize=0,total=0;
      for(const cut of message.cuts) total+=cut.chars.length;
      // Keep each host edit section small: AviUtl2 performs native timeline
      // mutations synchronously and large bursts can destabilize the editor.
      const batchLimit=4;
      for(const cut of message.cuts) for(let offset=0;offset<cut.chars.length;offset+=batchLimit) {
        const chars=cut.chars.slice(offset,offset+batchLimit);
        const part={...cut,glyphOffset:offset,glyphTotal:cut.chars.length,chars,advances:cut.advances.slice(offset,offset+batchLimit)};
        if(batchSize+chars.length>batchLimit&&batch.length){chunks.push(batch);batch=[];batchSize=0;}
        batch.push(part);batchSize+=chars.length;
      }
      if(batch.length)chunks.push(batch);
      if(!chunks.length) throw Error('ネイティブ化する歌詞がありません');
      const bar=document.getElementById('avi-progress'),label=document.getElementById('avi-status');
      setGenerating(true);bar.hidden=false;bar.value=0;
      let sent=0;
      const sendChunk=async i=>{
        const final=i===chunks.length-1;
        const part={...message,cuts:chunks[i],done:Math.min(total,sent+chunks[i].reduce((n,c)=>n+c.chars.length,0)),total,final,first:i===0};
        delete part.snapshot;delete part.pcm;
        label.textContent=`ネイティブレイヤー生成中 ${part.done}/${total} 文字（${i+1}/${chunks.length}）`;
        const ack=new Promise((resolve,reject)=>{nativeAckResolve=resolve;nativeAckReject=reject;});
        send(part);
        try {
          const result=await ack;sent=result.done;bar.value=total?sent/total*100:100;
          if(final){label.textContent='文字別編集レイヤーを生成しました';setGenerating(false);setTimeout(()=>{if(!generating)bar.hidden=true;},1800);}
          else setTimeout(()=>sendChunk(i+1),35);
        } catch(e) {setGenerating(false);bar.hidden=true;label.textContent='生成エラー: '+String(e.message||e);}
      };
      sendChunk(0);return;
    }
    if(sample){
      const ack=new Promise((resolve,reject)=>{nativeAckResolve=resolve;nativeAckReject=reject;});
      send(message);await ack;
      await addCutLayers();await switchCutMode();send({type:'saveSample'});
    }else if(!native){
      const ack=new Promise((resolve,reject)=>{nativeAckResolve=resolve;nativeAckReject=reject;});
      send(message);await ack;
    }
  }
  function cutNativeEdits(c,p,scheme) {
    const colors=p.colors||{};
    return {text:c.text||'',layout:c.layout,enter:c.enter,hold:c.hold,exit:c.exit,
      decor:(c.decor||[]).map(d=>d.id),treat:c.treat||'none',bg:c.bg||'none',cam:c.cam||'push',trans:c.trans||'なし',
      bpm:p.timing.bpm||0,fx:{...p.fx},fonts:{...p.fonts},colors:{enabled:!!colors.enabled,accentOn:!!colors.accentOn,
        bg:colors.bg||scheme.bg,fg:colors.fg||scheme.fg,sub:colors.sub||scheme.sub,
        accent:colors.accent||scheme.accent,ghostA:colors.ghostA||scheme.ghostA,ghostB:colors.ghostB||scheme.ghostB}};
  }
  async function addCutLayers() {
    if(generating)return;
    const p=JSON.parse(JSON.stringify(J.ui.project));
    const a=audioInfo(J.ui.audio),pl=J.plan(p,planningAudio(p,a));
    const renderSnapshot=enc(JSON.stringify({version:1,project:p,audio:a,fontFiles}));
    let pcm=null;
    if(J.ui.audio && p.includeAudio!==false) {
      const b=J.ui.audio.buffer,channels=Math.min(2,b.numberOfChannels),f=new Float32Array(b.length*channels);
      for(let c=0;c<channels;c++){const d=b.getChannelData(c);for(let i=0;i<b.length;i++)f[i*channels+c]=d[i];}
      pcm={rate:b.sampleRate,channels,data:bytes64(new Uint8Array(f.buffer))};
    }
    const all=pl.cuts||[];
    const valid=all.filter(c=>c.end>c.start);
    const scheme=J.resolveStyle(p).schemes[0];
    const cuts=valid.map((c,i)=>{
      const end=Math.min(c.end,valid[i+1]?.start??c.end);
      return {index:c.index,start:c.start,end,duration:end-c.start,edits:cutNativeEdits(c,p,scheme)};
    });
    if(!cuts.length)throw Error('生成できるカットがありません');
    const bar=document.getElementById('avi-progress'),label=document.getElementById('avi-status');
    setGenerating(true);bar.hidden=false;bar.value=0;
    try {
      for(let i=0;i<cuts.length;i++) {
        const part={type:'cut',cut:cuts[i],done:i+1,total:cuts.length,first:i===0,final:i===cuts.length-1};
        if(i===0){part.cutSnapshot=renderSnapshot;part.pcm=pcm;part.audioData=audioData;}
        label.textContent=`カット別映像レイヤー生成中 ${i+1}/${cuts.length}`;
        const ack=new Promise((resolve,reject)=>{nativeAckResolve=resolve;nativeAckReject=reject;});
        send(part);await ack;
        bar.value=(i+1)/cuts.length*100;
        if(i+1<cuts.length)await new Promise(resolve=>setTimeout(resolve,100));
      }
      label.textContent='カット別映像レイヤーを生成しました';
    } finally {
      setGenerating(false);
      setTimeout(()=>{if(!generating)bar.hidden=true;},1800);
    }
  }
  async function switchCutMode() {
    if(generating)return;
    const bar=document.getElementById('avi-progress'),label=document.getElementById('avi-status');
    const sendStep=async(step,done,total)=>{
      const ack=new Promise((resolve,reject)=>{nativeAckResolve=resolve;nativeAckReject=reject;});
      send({type:'mode',...step,done,total});
      return ack;
    };
    setGenerating(true);bar.hidden=false;bar.value=0;
    try {
      label.textContent='カットを主映像として配置中';
      await sendStep({stage:'source',enabled:true},1,1);
      bar.value=100;
      cutMode=true;
      label.textContent='カットを主映像として配置しました';
    } catch(error) {
      try {await sendStep({stage:'source',enabled:false},0,1);}catch(_){}
      label.textContent='切り替えエラー: '+String(error.message||error);
      send({type:'error',error:label.textContent});
    } finally {
      setGenerating(false);
      setTimeout(()=>{if(!generating)bar.hidden=true;},1800);
    }
  }
  async function applyMain(bpmOverride=null,densityOverride=null) {
    if(generating)return;
    const bpm=bpmOverride!==null?bpmOverride:await new Promise(resolve=>{
      commonBpmResolve=resolve;send({type:'commonBpm'});
      setTimeout(()=>{if(commonBpmResolve===resolve){commonBpmResolve=null;resolve(null);}},3000);
    });
    if((bpm!==null&&Number.isFinite(bpm)&&J.ui.project.timing.bpm!==bpm)||
       (densityOverride!==null&&Number.isFinite(densityOverride)&&J.ui.project.fx.density!==densityOverride)){
      const p=JSON.parse(JSON.stringify(J.ui.project));
      if(bpm!==null&&Number.isFinite(bpm))p.timing.bpm=bpm;
      if(densityOverride!==null&&Number.isFinite(densityOverride))p.fx.density=densityOverride;
      J.aviSetProject(p,J.ui.audio);
    }
    await addCutLayers();
    await switchCutMode();
  }
  window.aviLoadAndApply=async (encoded,common)=>{
    await window.aviLoad(encoded);
    const p=JSON.parse(JSON.stringify(J.ui.project));
    p.fx={...p.fx,...common.fx};p.colors={...p.colors,...common.colors};p.fonts={...p.fonts,...common.fonts};
    if(Number.isFinite(common.bpm))p.timing.bpm=common.bpm;
    J.aviSetProject(p,J.ui.audio);
    await applyMain(Number.isFinite(common.bpm)?common.bpm:null,Number.isFinite(common.fx?.density)?common.fx.density:null);
  };
  window.aviSample=()=>snapshot(true);
  window.aviApply=()=>applyMain();
  function init() {
    if(!J.ui) { send({type:'ready',engine:true}); return; }
    const bar=document.createElement('div');
    bar.style.cssText='position:fixed;z-index:100000;left:0;right:0;bottom:0;padding:7px 12px;background:#202126;color:#fff;border-top:1px solid #666;display:grid;grid-template-columns:minmax(0,1fr);gap:5px;font:12px sans-serif;box-sizing:border-box';
    bar.innerHTML='<div style="display:flex;flex-wrap:wrap;gap:6px"><button id="avi-apply" title="カットを生成し主映像として適用">適用</button><button id="avi-native" title="カットの時間に合わせて文字別のAviUtl2テキストを生成">文字レイヤー</button></div><div style="display:flex;align-items:center;gap:8px;min-width:0"><progress id="avi-progress" max="100" value="0" hidden style="width:140px;flex:0 0 140px"></progress><span id="avi-status" style="white-space:nowrap;overflow:hidden;text-overflow:ellipsis;flex:1">JIZURA — AviUtl2内で編集・描画</span></div>';
    document.body.appendChild(bar); document.body.style.paddingBottom='86px';
    document.getElementById('avi-apply').onclick=()=>applyMain().catch(e=>{setGenerating(false);send({type:'error',error:String(e)});});
    document.getElementById('avi-native').onclick=()=>snapshot(false,true).catch(e=>{setGenerating(false);send({type:'error',error:String(e)});});
    send({type:'ready',engine:false});
  }
  if(document.readyState==='loading')document.addEventListener('DOMContentLoaded',init);else init();
})();
