// COM/DirectShow registration of "Lensbridge Camera" in the video input
// device category (what Zoom, Discord, OBS, Chrome enumerate as webcams).

#include "vcam.h"

extern "C" const GUID CLSID_LensbridgeVCam = {0xcb4e2a03, 0x21da, 0x4355, {0x82, 0x13, 0xe8, 0xe2, 0xe5, 0x8e, 0xaf, 0x82}};

namespace {

const AMOVIESETUP_MEDIATYPE kPinTypes = {&MEDIATYPE_Video, &MEDIASUBTYPE_NULL};

const AMOVIESETUP_PIN kPin = {
    const_cast<LPWSTR>(L"Capture"),  // name (obsolete)
    FALSE,                           // rendered
    TRUE,                            // output
    FALSE,                           // zero instances allowed
    FALSE,                           // many instances allowed
    &CLSID_NULL,                     // connects to filter
    nullptr,                         // connects to pin
    1,
    &kPinTypes,
};

const AMOVIESETUP_FILTER kFilter = {&CLSID_LensbridgeVCam, LB_VCAM_FRIENDLY_NAME, MERIT_DO_NOT_USE, 1, &kPin};

}  // namespace

// Defined in the base classes (dllsetup.cpp) without a header declaration.
STDAPI AMovieSetupRegisterServer(CLSID clsServer, LPCWSTR szDescription, LPCWSTR szFileName, LPCWSTR szThreadingModel,
                                 LPCWSTR szServerType);
STDAPI AMovieSetupUnregisterServer(CLSID clsServer);

CFactoryTemplate g_Templates[] = {
    {LB_VCAM_FRIENDLY_NAME, &CLSID_LensbridgeVCam, CVCam::CreateInstance, nullptr, &kFilter},
};
int g_cTemplates = sizeof(g_Templates) / sizeof(g_Templates[0]);

static HRESULT RegisterFilters(BOOL reg) {
    WCHAR path[MAX_PATH] = {};
    if (!GetModuleFileNameW(g_hInst, path, MAX_PATH)) return HRESULT_FROM_WIN32(GetLastError());

    HRESULT hr = CoInitialize(nullptr);
    const bool uninit = SUCCEEDED(hr);
    if (reg) hr = AMovieSetupRegisterServer(CLSID_LensbridgeVCam, LB_VCAM_FRIENDLY_NAME, path, L"Both", L"InprocServer32");
    else hr = S_OK;

    if (SUCCEEDED(hr)) {
        IFilterMapper2* fm = nullptr;
        hr = CoCreateInstance(CLSID_FilterMapper2, nullptr, CLSCTX_INPROC_SERVER, IID_IFilterMapper2,
                              reinterpret_cast<void**>(&fm));
        if (SUCCEEDED(hr)) {
            if (reg) {
                IMoniker* moniker = nullptr;
                REGFILTER2 rf2 = {};
                rf2.dwVersion = 1;
                rf2.dwMerit = MERIT_DO_NOT_USE;
                rf2.cPins = 1;
                rf2.rgPins = &kPin;
                hr = fm->RegisterFilter(CLSID_LensbridgeVCam, LB_VCAM_FRIENDLY_NAME, &moniker,
                                        &CLSID_VideoInputDeviceCategory, nullptr, &rf2);
                if (moniker) moniker->Release();
            } else {
                hr = fm->UnregisterFilter(&CLSID_VideoInputDeviceCategory, nullptr, CLSID_LensbridgeVCam);
            }
            fm->Release();
        }
    }
    if (!reg) {
        const HRESULT hr2 = AMovieSetupUnregisterServer(CLSID_LensbridgeVCam);
        if (SUCCEEDED(hr)) hr = hr2;
    }
    CoFreeUnusedLibraries();
    if (uninit) CoUninitialize();
    return hr;
}

STDAPI DllRegisterServer() { return RegisterFilters(TRUE); }

STDAPI DllUnregisterServer() { return RegisterFilters(FALSE); }

extern "C" BOOL WINAPI DllEntryPoint(HINSTANCE, ULONG, LPVOID);

BOOL APIENTRY DllMain(HANDLE module, DWORD reason, LPVOID reserved) {
    return DllEntryPoint(static_cast<HINSTANCE>(module), reason, reserved);
}
