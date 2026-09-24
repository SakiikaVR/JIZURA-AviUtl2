#define NOMINMAX
#include <windows.h>
#include <commctrl.h>
#include <wrl.h>
#include <wrl/event.h>
#include <shlwapi.h>
#include <d3d11.h>
#include <DirectXMath.h>
#include <filesystem>
#include <fstream>
#include <thread>
#include <mutex>
#include <condition_variable>
#include <deque>
#include <memory>
#include <atomic>
#include <cmath>
#include "WebView2.h"
#include "WebView2EnvironmentOptions.h"


#include "json.hpp"
using Microsoft::WRL::ComPtr;
using Microsoft::WRL::Callback;
using json=nlohmann::json;
namespace fs=std::filesystem;
static HINSTANCE instance;
static fs::path base;
static HWND panel;
static HWND mainWindow=nullptr;
static bool previewActive=false;
static bool cursorCorrected=false;
static void log(const std::string&s);

static ComPtr<ICoreWebView2Controller> panelController;
static ComPtr<ICoreWebView2> panelWeb;
static bool panelReady=false;

static std::string pendingLoad;

static constexpr UINT INIT_WEB=WM_APP+31, MAKE_SAMPLE=WM_APP+32, SAVE_SAMPLE=WM_APP+33, PREVIEW_STATE=WM_APP+34;
static LRESULT CALLBACK mainCursorProc(HWND h,UINT m,WPARAM w,LPARAM l,UINT_PTR id,DWORD_PTR){
    if(m==WM_NCDESTROY){RemoveWindowSubclass(h,mainCursorProc,id);if(mainWindow==h)mainWindow=nullptr;return DefSubclassProc(h,m,w,l);}
    LRESULT result=DefSubclassProc(h,m,w,l);
    if(previewActive && (m==WM_SETCURSOR || m==WM_MOUSEMOVE)){
        CURSORINFO info{sizeof(info)};
        if(GetCursorInfo(&info) && info.hCursor==LoadCursor(nullptr,IDC_APPSTARTING)){
            SetCursor(LoadCursor(nullptr,IDC_ARROW));
            if(!cursorCorrected){cursorCorrected=true;log("Corrected preview busy cursor");}
            if(m==WM_SETCURSOR)return TRUE;
        }
    }
    return result;
}
static void attachMainCursor(){
    if(mainWindow&&IsWindow(mainWindow))return;
    struct Search {HWND found=nullptr;LONG area=0;} search;
    EnumThreadWindows(GetCurrentThreadId(),[](HWND h,LPARAM p)->BOOL{
        auto*s=reinterpret_cast<Search*>(p);wchar_t name[64]={};GetClassNameW(h,name,64);
        if(wcscmp(name,L"aviutl2Manager")!=0)return TRUE;
        RECT r{};if(!GetWindowRect(h,&r))return TRUE;
        LONG area=(r.right-r.left)*(r.bottom-r.top);
        if(area>s->area){s->area=area;s->found=h;}return TRUE;
    },reinterpret_cast<LPARAM>(&search));
    if(search.found && SetWindowSubclass(search.found,mainCursorProc,1,0))mainWindow=search.found;
}

static std::wstring wide(const std::string&s){ if(s.empty())return {};int n=MultiByteToWideChar(CP_UTF8,0,s.data(),(int)s.size(),nullptr,0);std::wstring r(n,0);MultiByteToWideChar(CP_UTF8,0,s.data(),(int)s.size(),r.data(),n);return r; }
static std::string utf8(const std::wstring&s){if(s.empty())return {};int n=WideCharToMultiByte(CP_UTF8,0,s.data(),(int)s.size(),nullptr,0,nullptr,nullptr);std::string r(n,0);WideCharToMultiByte(CP_UTF8,0,s.data(),(int)s.size(),r.data(),n,nullptr,nullptr);return r;}
static void log(const std::string&s){try{std::ofstream f(base/L"JIZURA.log",std::ios::app);f<<s<<"\n";}catch(...){}}
static void check(HRESULT h){if(FAILED(h))throw std::runtime_error("HRESULT "+std::to_string((unsigned long)h));}
static std::wstring jsarg(const std::string&s){return wide(json(s).dump());}
static fs::path profile(const wchar_t* suffix){wchar_t p[32768];GetEnvironmentVariableW(L"LOCALAPPDATA",p,32768);return fs::path(p)/L"JIZURA-AviUtl2"/suffix;}
static void webSetup(ICoreWebView2* w,const wchar_t* page){
    ComPtr<ICoreWebView2_3> w3;check(w->QueryInterface(IID_PPV_ARGS(&w3)));
    check(w3->SetVirtualHostNameToFolderMapping(L"jizura.local",(base/L"web").c_str(),COREWEBVIEW2_HOST_RESOURCE_ACCESS_KIND_DENY_CORS));
    // Keep the native bridge confined to the bundled application.
    EventRegistrationToken tok;
    w->add_NavigationStarting(Callback<ICoreWebView2NavigationStartingEventHandler>([](ICoreWebView2*,ICoreWebView2NavigationStartingEventArgs*a)->HRESULT{LPWSTR p=nullptr;a->get_Uri(&p);std::wstring u=p?p:L"";CoTaskMemFree(p);if(u.rfind(L"https://jizura.local/",0)!=0)a->put_Cancel(TRUE);return S_OK;}).Get(),&tok);
    w->add_NewWindowRequested(Callback<ICoreWebView2NewWindowRequestedEventHandler>([](ICoreWebView2*,ICoreWebView2NewWindowRequestedEventArgs*a)->HRESULT{a->put_Handled(TRUE);return S_OK;}).Get(),&tok);
    check(w->Navigate((std::wstring(L"https://jizura.local/")+page).c_str()));
}
static json webMessage(ICoreWebView2WebMessageReceivedEventArgs*a){LPWSTR p=nullptr;check(a->get_Source(&p));std::wstring source=p?p:L"";CoTaskMemFree(p);if(source.rfind(L"https://jizura.local/",0)!=0)throw std::runtime_error("Invalid message source");p=nullptr;check(a->TryGetWebMessageAsString(&p));std::string s=utf8(p?p:L"");CoTaskMemFree(p);return json::parse(s);}
struct Frame {int w=0,h=0;std::vector<BYTE> pixels;};
struct Job {std::string snapshot,error;double t=0;bool transparent=false,done=false;uint64_t id=0;Frame frame;std::mutex mutex;std::condition_variable cv;};
class Renderer {
    std::thread thread;std::mutex mutex;std::deque<std::shared_ptr<Job>> queue;
    std::shared_ptr<Job> active;std::atomic<bool> stopping{false};
    HWND hwnd=nullptr;ComPtr<ICoreWebView2Controller> controller;ComPtr<ICoreWebView2> web;
    ComPtr<ICoreWebView2Environment12> environment;
    ComPtr<ICoreWebView2SharedBuffer> sharedBuffer;BYTE* sharedData=nullptr;size_t sharedSize=0;bool sharedReady=false;
    bool ready=false;std::string startupError;uint64_t next=1;
    void finish(std::shared_ptr<Job>j,const std::string&e){std::lock_guard<std::mutex>l(j->mutex);j->error=e;j->done=true;j->cv.notify_all();}
    void allocateBuffer(size_t bytes){
        if(!environment)throw std::runtime_error("WebView2 shared buffers are unavailable");
        if(bytes==0||bytes>8192ull*8192ull*4ull)throw std::runtime_error("Invalid shared frame size");
        ComPtr<ICoreWebView2SharedBuffer> nextBuffer;
        check(environment->CreateSharedBuffer(bytes,&nextBuffer));
        BYTE* nextData=nullptr;check(nextBuffer->get_Buffer(&nextData));
        if(web){
            ComPtr<ICoreWebView2_17> w17;check(web.As(&w17));
            check(w17->PostSharedBufferToScript(nextBuffer.Get(),COREWEBVIEW2_SHARED_BUFFER_ACCESS_READ_WRITE,L"{\"type\":\"framebuffer\"}"));
        }
        if(sharedBuffer)sharedBuffer->Close();
        sharedBuffer=nextBuffer;sharedData=nextData;sharedSize=bytes;sharedReady=false;
    }
    void tick(){
        if(stopping){if(active){finish(active,"Renderer stopped");active.reset();}std::lock_guard<std::mutex>l(mutex);for(auto&j:queue)finish(j,"Renderer stopped");queue.clear();PostQuitMessage(0);return;}
        if(active)return;
        {std::lock_guard<std::mutex>l(mutex);if(queue.empty()||(!sharedReady&&startupError.empty()))return;active=queue.front();queue.pop_front();}
        if(!startupError.empty()){finish(active,startupError);active.reset();return;}
        auto j=active;std::wstring script=L"aviRender("+std::to_wstring(j->id)+L","+jsarg(j->snapshot)+L","+std::to_wstring(j->t)+L","+(j->transparent?L"true":L"false")+L")";
        HRESULT hr=web->ExecuteScript(script.c_str(),Callback<ICoreWebView2ExecuteScriptCompletedHandler>([this,j](HRESULT h,LPCWSTR)->HRESULT{if(FAILED(h)&&active==j){finish(j,"ExecuteScript failed");active.reset();}return S_OK;}).Get());
        if(FAILED(hr)){finish(j,"ExecuteScript failed");active.reset();}
    }
    static LRESULT CALLBACK proc(HWND h,UINT m,WPARAM w,LPARAM l){auto self=(Renderer*)GetWindowLongPtr(h,GWLP_USERDATA);if(m==WM_NCCREATE){self=(Renderer*)((CREATESTRUCT*)l)->lpCreateParams;SetWindowLongPtr(h,GWLP_USERDATA,(LONG_PTR)self);}if(self&&m==WM_TIMER){self->tick();return 0;}return DefWindowProc(h,m,w,l);}
    void run(){
        CoInitializeEx(nullptr,COINIT_APARTMENTTHREADED);
        WNDCLASS wc{};wc.lpfnWndProc=proc;wc.hInstance=instance;wc.lpszClassName=L"JIZURA_Renderer";RegisterClass(&wc);
        hwnd=CreateWindowEx(0,wc.lpszClassName,L"JIZURA render",WS_POPUP,0,0,1920,1080,nullptr,nullptr,instance,this);SetTimer(hwnd,1,5,nullptr);
        auto options=Microsoft::WRL::Make<CoreWebView2EnvironmentOptions>();
        options->put_AdditionalBrowserArguments(L"--disable-background-timer-throttling --disable-renderer-backgrounding --disable-accelerated-2d-canvas");
        HRESULT hr=CreateCoreWebView2EnvironmentWithOptions(nullptr,profile(L"Renderer").c_str(),options.Get(),Callback<ICoreWebView2CreateCoreWebView2EnvironmentCompletedHandler>([this](HRESULT h,ICoreWebView2Environment*env)->HRESULT{
            if(FAILED(h)||!env){startupError="WebView2 environment failed: "+std::to_string((unsigned long)h);return S_OK;}
            if(FAILED(env->QueryInterface(IID_PPV_ARGS(&environment)))){startupError="WebView2 shared buffers are unavailable";return S_OK;}
            try{allocateBuffer(1920ull*1080ull*4ull);}catch(const std::exception&e){startupError=e.what();return S_OK;}
            return env->CreateCoreWebView2Controller(hwnd,Callback<ICoreWebView2CreateCoreWebView2ControllerCompletedHandler>([this](HRESULT h,ICoreWebView2Controller*c)->HRESULT{
                if(FAILED(h)||!c){startupError="WebView2 controller failed";return S_OK;}
                controller=c;c->get_CoreWebView2(&web);RECT r{0,0,1920,1080};c->put_Bounds(r);c->put_IsVisible(TRUE);
                EventRegistrationToken tok;
                web->add_WebMessageReceived(Callback<ICoreWebView2WebMessageReceivedEventHandler>([this](ICoreWebView2*,ICoreWebView2WebMessageReceivedEventArgs*a)->HRESULT{
                    try{
                        auto m=webMessage(a);auto type=m.value("type","");
                        if(type=="ready"){
                            ready=true;log("Renderer ready");
                            ComPtr<ICoreWebView2_17> w17;check(web.As(&w17));
                            check(w17->PostSharedBufferToScript(sharedBuffer.Get(),COREWEBVIEW2_SHARED_BUFFER_ACCESS_READ_WRITE,L"{\"type\":\"framebuffer\"}"));
                        } else if(type=="bufferReady"){
                            sharedReady=true;log("Shared frame buffer ready");
                        } else if(active&&m.value("id",uint64_t(0))==active->id){
                            auto j=active;
                            if(type=="bufferNeeded"){
                                auto bytes=m.at("bytes").get<size_t>();
                                allocateBuffer(bytes);
                            } else if(type=="frameRaw"){
                                if(m.contains("report")&&!m["report"].is_null()){
                                    auto report=m["report"];std::string group=report.value("group","unknown");
                                    if(group.find_first_not_of("abcdefghijklmnopqrstuvwxyz")==std::string::npos){std::ofstream f(base/("audit-"+group+".json"));f<<report.dump(2);}
                                }
                                auto transferStart=GetTickCount64();
                                int w=m.at("w"),h=m.at("h");size_t count=(size_t)w*h*4;
                                if(!sharedReady||!sharedData||w<=0||h<=0||w>8192||h>8192||count>sharedSize)throw std::runtime_error("Invalid shared frame");
                                j->frame.w=w;j->frame.h=h;j->frame.pixels.assign(sharedData,sharedData+count);
                                // AviUtl2 expects premultiplied RGBA.
                                for(size_t i=0;i<count;i+=4){auto alpha=j->frame.pixels[i+3];for(int k=0;k<3;k++)j->frame.pixels[i+k]=(BYTE)((j->frame.pixels[i+k]*alpha+127)/255);}
                                if(GetEnvironmentVariableW(L"JIZURA_PROFILE",nullptr,0)>0&&m.contains("profile")){
                                    auto p=m["profile"];
                                    log("profile raw draw="+std::to_string(p.value("drawMs",0.0))+" transfer="+std::to_string(p.value("transferMs",0.0))+" copy="+std::to_string(GetTickCount64()-transferStart));
                                }
                                finish(j,"");active.reset();
                            } else {finish(j,m.value("error","Render failed"));active.reset();}
                        }
                    }catch(const std::exception&e){if(active){finish(active,e.what());active.reset();}else {startupError=e.what();log(e.what());}}return S_OK;
                }).Get(),&tok);
                try{webSetup(web.Get(),L"render.html");}catch(const std::exception&e){startupError=e.what();}return S_OK;
            }).Get());
        }).Get());
        if(FAILED(hr))startupError="WebView2 runtime unavailable";
        MSG msg;while(GetMessage(&msg,nullptr,0,0)>0){TranslateMessage(&msg);DispatchMessage(&msg);}
        if(controller)controller->Close();web.Reset();controller.Reset();if(sharedBuffer)sharedBuffer->Close();sharedBuffer.Reset();environment.Reset();sharedData=nullptr;DestroyWindow(hwnd);CoUninitialize();
    }
public:
    Renderer(){thread=std::thread([this]{run();});}
    ~Renderer(){stopping=true;if(thread.joinable())thread.join();}
    Frame render(const std::string&s,double t,bool transparent){auto j=std::make_shared<Job>();j->snapshot=s;j->t=t;j->transparent=transparent;{std::lock_guard<std::mutex>l(mutex);j->id=next++;queue.push_back(j);}std::unique_lock<std::mutex>l(j->mutex);if(!j->cv.wait_for(l,std::chrono::seconds(60),[&]{return j->done;}))throw std::runtime_error("JIZURA rendering timed out");if(!j->error.empty())throw std::runtime_error(j->error);return std::move(j->frame);}
};
// C ABI transport only. AviUtl2 registration, editing and persistence live in Rust.
static std::unique_ptr<Renderer> renderHost;
static std::mutex renderLock;
static void (*onMessage)(const char*)=nullptr;
static thread_local std::string ffiError;
static LRESULT CALLBACK editorProc(HWND h,UINT m,WPARAM w,LPARAM l);
extern "C" const char* jz_error(){return ffiError.c_str();}
extern "C" void jz_init(const wchar_t* directory){
    base=directory;GetModuleHandleEx(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS|GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,(LPCWSTR)&jz_init,&instance);
}
extern "C" Frame* jz_render(const char* snapshot,double t,bool transparent){
    try{std::lock_guard<std::mutex>l(renderLock);if(!renderHost)renderHost=std::make_unique<Renderer>();return new Frame(renderHost->render(snapshot,t,transparent));}
    catch(const std::exception&e){ffiError=e.what();log(ffiError);return nullptr;}
}
extern "C" const BYTE* jz_pixels(Frame*f,int*w,int*h){*w=f->w;*h=f->h;return f->pixels.data();}
extern "C" void jz_free_frame(Frame*f){delete f;}
extern "C" void jz_exec(const char* script){if(panelWeb)panelWeb->ExecuteScript(wide(script).c_str(),nullptr);}
extern "C" void jz_load(const char*s){pendingLoad=s;if(panelReady){panelWeb->ExecuteScript((L"aviLoad("+jsarg(pendingLoad)+L")").c_str(),nullptr);pendingLoad.clear();}ShowWindow(panel,SW_SHOW);}
extern "C" void jz_status(const char*s){if(panelWeb)panelWeb->ExecuteScript((L"document.getElementById('avi-status').textContent="+jsarg(s)).c_str(),nullptr);log(s);}
static void createEditor(){
    auto options=Microsoft::WRL::Make<CoreWebView2EnvironmentOptions>();
    CreateCoreWebView2EnvironmentWithOptions(nullptr,profile(L"Editor").c_str(),options.Get(),Callback<ICoreWebView2CreateCoreWebView2EnvironmentCompletedHandler>([](HRESULT h,ICoreWebView2Environment*e)->HRESULT{
        if(FAILED(h)||!e){log("Editor WebView2 environment failed");return S_OK;}
        return e->CreateCoreWebView2Controller(panel,Callback<ICoreWebView2CreateCoreWebView2ControllerCompletedHandler>([](HRESULT h,ICoreWebView2Controller*c)->HRESULT{
            if(FAILED(h)||!c){log("Editor controller failed");return S_OK;}panelController=c;c->get_CoreWebView2(&panelWeb);RECT r;GetClientRect(panel,&r);c->put_Bounds(r);
            EventRegistrationToken tok;
            panelWeb->add_WebMessageReceived(Callback<ICoreWebView2WebMessageReceivedEventHandler>([](ICoreWebView2*,ICoreWebView2WebMessageReceivedEventArgs*a)->HRESULT{
                try{auto m=webMessage(a);if(m.value("type","")=="ready"){panelReady=true;log("Editor ready");if(!pendingLoad.empty()){auto s=pendingLoad;jz_load(s.c_str());}}
                    if(onMessage){auto s=m.dump();onMessage(s.c_str());}
                }catch(const std::exception&e){log(e.what());}return S_OK;
            }).Get(),&tok);
            try{webSetup(panelWeb.Get(),L"editor.html");}catch(const std::exception&e){log(e.what());}return S_OK;
        }).Get());
    }).Get());
}
static LRESULT CALLBACK editorProc(HWND h,UINT m,WPARAM w,LPARAM l){
    if(m==INIT_WEB){createEditor();return 0;}
    if(m==PREVIEW_STATE){previewActive=w!=0;if(previewActive)attachMainCursor();return 0;}
    if(m==WM_SIZE&&panelController){RECT r;GetClientRect(h,&r);panelController->put_Bounds(r);return 0;}
    if(m==MAKE_SAMPLE){if(panelReady)jz_exec("aviSample()");return 0;}
    if(m==SAVE_SAMPLE){if(onMessage)onMessage("{\"type\":\"saveSample\"}");return 0;}
    return DefWindowProc(h,m,w,l);
}
extern "C" HWND jz_panel(void(*callback)(const char*)){
    onMessage=callback;WNDCLASS wc{};wc.lpfnWndProc=editorProc;wc.hInstance=instance;wc.lpszClassName=L"JIZURA_Editor";wc.hCursor=LoadCursor(nullptr,IDC_ARROW);RegisterClass(&wc);
    panel=CreateWindowEx(0,wc.lpszClassName,L"JIZURA",WS_POPUP,0,0,1200,800,nullptr,nullptr,instance,nullptr);PostMessage(panel,INIT_WEB,0,0);return panel;
}
extern "C" void jz_sample(){PostMessage(panel,MAKE_SAMPLE,0,0);}
extern "C" void jz_preview_state(bool active){PostMessage(panel,PREVIEW_STATE,active?1:0,0);}
extern "C" void jz_defer_save(){PostMessage(panel,SAVE_SAMPLE,0,0);}
extern "C" void jz_shutdown(){renderHost.reset();if(panelController)panelController->Close();panelWeb.Reset();panelController.Reset();if(panel)DestroyWindow(panel);panel=nullptr;panelReady=false;}
