#include "vcam.h"

#include <mmsystem.h>
#include <new>

namespace {

struct Size {
    int w, h;
};

// First entry is the default format.
const Size kSizes[] = {{1280, 720}, {1920, 1080}, {960, 540}, {640, 360}, {640, 480}, {320, 240}};
constexpr int kNumSizes = sizeof(kSizes) / sizeof(kSizes[0]);
constexpr int kNumSubtypes = 2;  // YUY2, RGB24
constexpr int kNumFormats = kNumSizes * kNumSubtypes;

constexpr REFERENCE_TIME kDefaultFrameTime = 333333;  // 30 fps
constexpr REFERENCE_TIME kMinFrameTime = 166666;      // 60 fps
constexpr REFERENCE_TIME kMaxFrameTime = 2000000;     // 5 fps

const GUID& SubtypeAt(int index) { return index < kNumSizes ? MEDIASUBTYPE_YUY2 : MEDIASUBTYPE_RGB24; }

OutFormat OutFormatOf(const GUID& subtype) { return subtype == MEDIASUBTYPE_YUY2 ? OutFormat::YUY2 : OutFormat::RGB24; }

HRESULT BuildMediaType(int index, CMediaType* pmt, REFERENCE_TIME frame_time) {
    if (index < 0 || index >= kNumFormats) return VFW_S_NO_MORE_ITEMS;
    const Size s = kSizes[index % kNumSizes];
    const GUID& subtype = SubtypeAt(index);

    auto* pvi = reinterpret_cast<VIDEOINFOHEADER*>(pmt->AllocFormatBuffer(sizeof(VIDEOINFOHEADER)));
    if (!pvi) return E_OUTOFMEMORY;
    ZeroMemory(pvi, sizeof(VIDEOINFOHEADER));
    pvi->bmiHeader.biSize = sizeof(BITMAPINFOHEADER);
    pvi->bmiHeader.biWidth = s.w;
    pvi->bmiHeader.biHeight = s.h;
    pvi->bmiHeader.biPlanes = 1;
    if (subtype == MEDIASUBTYPE_YUY2) {
        pvi->bmiHeader.biBitCount = 16;
        pvi->bmiHeader.biCompression = MAKEFOURCC('Y', 'U', 'Y', '2');
    } else {
        pvi->bmiHeader.biBitCount = 24;
        pvi->bmiHeader.biCompression = BI_RGB;
    }
    pvi->bmiHeader.biSizeImage = static_cast<DWORD>(FrameBytes(s.w, s.h, OutFormatOf(subtype)));
    pvi->AvgTimePerFrame = frame_time;
    pvi->dwBitRate = static_cast<DWORD>(static_cast<LONGLONG>(pvi->bmiHeader.biSizeImage) * 8 * UNITS / frame_time);
    SetRectEmpty(&pvi->rcSource);
    SetRectEmpty(&pvi->rcTarget);

    pmt->SetType(&MEDIATYPE_Video);
    pmt->SetFormatType(&FORMAT_VideoInfo);
    pmt->SetTemporalCompression(FALSE);
    pmt->SetSubtype(&subtype);
    pmt->SetSampleSize(pvi->bmiHeader.biSizeImage);
    return S_OK;
}

// Returns the format index matching size + subtype, or -1.
int FindFormat(const AM_MEDIA_TYPE* pmt) {
    if (!pmt || pmt->majortype != MEDIATYPE_Video || pmt->formattype != FORMAT_VideoInfo || !pmt->pbFormat ||
        pmt->cbFormat < sizeof(VIDEOINFOHEADER))
        return -1;
    const auto* pvi = reinterpret_cast<const VIDEOINFOHEADER*>(pmt->pbFormat);
    for (int i = 0; i < kNumFormats; ++i) {
        const Size s = kSizes[i % kNumSizes];
        if (pmt->subtype == SubtypeAt(i) && pvi->bmiHeader.biWidth == s.w && pvi->bmiHeader.biHeight == s.h) return i;
    }
    return -1;
}

LONGLONG Now100ns() {
    static LARGE_INTEGER freq = [] {
        LARGE_INTEGER f;
        QueryPerformanceFrequency(&f);
        return f;
    }();
    LARGE_INTEGER c;
    QueryPerformanceCounter(&c);
    return static_cast<LONGLONG>(static_cast<double>(c.QuadPart) * 10000000.0 / static_cast<double>(freq.QuadPart));
}

}  // namespace

// ---------------------------------------------------------------- CVCam

CUnknown* WINAPI CVCam::CreateInstance(LPUNKNOWN lpunk, HRESULT* phr) {
    CUnknown* punk = new (std::nothrow) CVCam(lpunk, phr);
    if (!punk && phr) *phr = E_OUTOFMEMORY;
    return punk;
}

CVCam::CVCam(LPUNKNOWN lpunk, HRESULT* phr) : CSource(NAME("Lensbridge Camera"), lpunk, CLSID_LensbridgeVCam) {
    CAutoLock lock(&m_cStateLock);
    // The CSourceStream constructor registers the pin with this filter (AddPin).
    new CVCamStream(phr, this, L"Capture");
}

CVCamStream* CVCam::Stream() const { return m_iPins > 0 ? static_cast<CVCamStream*>(m_paStreams[0]) : nullptr; }

STDMETHODIMP CVCam::NonDelegatingQueryInterface(REFIID riid, void** ppv) {
    if (riid == IID_IAMFilterMiscFlags) return GetInterface(static_cast<IAMFilterMiscFlags*>(this), ppv);
    // Some apps (Skype, older Zoom) look for these on the filter instead of the pin.
    if ((riid == IID_IAMStreamConfig || riid == IID_IKsPropertySet) && Stream())
        return Stream()->NonDelegatingQueryInterface(riid, ppv);
    return CSource::NonDelegatingQueryInterface(riid, ppv);
}

// ---------------------------------------------------------------- CVCamStream

CVCamStream::CVCamStream(HRESULT* phr, CVCam* parent, LPCWSTR pin_name)
    : CSourceStream(NAME("Lensbridge Camera Stream"), phr, parent, pin_name), parent_(parent) {
    BuildMediaType(0, &m_mt, kDefaultFrameTime);
}

STDMETHODIMP CVCamStream::NonDelegatingQueryInterface(REFIID riid, void** ppv) {
    if (riid == IID_IAMStreamConfig) return GetInterface(static_cast<IAMStreamConfig*>(this), ppv);
    if (riid == IID_IKsPropertySet) return GetInterface(static_cast<IKsPropertySet*>(this), ppv);
    return CSourceStream::NonDelegatingQueryInterface(riid, ppv);
}

HRESULT CVCamStream::GetMediaType(int iPosition, CMediaType* pmt) {
    CheckPointer(pmt, E_POINTER);
    CAutoLock lock(m_pFilter->pStateLock());
    if (iPosition < 0) return E_INVALIDARG;
    // Offer the format chosen through IAMStreamConfig first, so connection honours it.
    if (format_set_) {
        if (iPosition == 0) {
            *pmt = m_mt;
            return S_OK;
        }
        return BuildMediaType(iPosition - 1, pmt, kDefaultFrameTime);
    }
    return BuildMediaType(iPosition, pmt, kDefaultFrameTime);
}

HRESULT CVCamStream::CheckMediaType(const CMediaType* pMediaType) {
    if (FindFormat(pMediaType) < 0) return E_INVALIDARG;
    const auto* pvi = reinterpret_cast<const VIDEOINFOHEADER*>(pMediaType->Format());
    if (pvi->AvgTimePerFrame != 0 && (pvi->AvgTimePerFrame < kMinFrameTime || pvi->AvgTimePerFrame > kMaxFrameTime))
        return E_INVALIDARG;
    return S_OK;
}

HRESULT CVCamStream::SetMediaType(const CMediaType* pmt) {
    CAutoLock lock(m_pFilter->pStateLock());
    const int index = FindFormat(pmt);
    if (index < 0) return E_INVALIDARG;
    const auto* pvi = reinterpret_cast<const VIDEOINFOHEADER*>(pmt->Format());
    const REFERENCE_TIME ft = pvi->AvgTimePerFrame ? pvi->AvgTimePerFrame : kDefaultFrameTime;
    CMediaType normalized;
    HRESULT hr = BuildMediaType(index, &normalized, ft);
    if (FAILED(hr)) return hr;
    return CSourceStream::SetMediaType(&normalized);
}

HRESULT CVCamStream::DecideBufferSize(IMemAllocator* pAlloc, ALLOCATOR_PROPERTIES* pRequest) {
    CheckPointer(pAlloc, E_POINTER);
    CheckPointer(pRequest, E_POINTER);
    CAutoLock lock(m_pFilter->pStateLock());
    const auto* pvi = reinterpret_cast<const VIDEOINFOHEADER*>(m_mt.Format());
    if (pRequest->cBuffers < 1) pRequest->cBuffers = 1;
    pRequest->cbBuffer = pvi->bmiHeader.biSizeImage;
    if (pRequest->cbAlign < 1) pRequest->cbAlign = 1;
    ALLOCATOR_PROPERTIES actual;
    HRESULT hr = pAlloc->SetProperties(pRequest, &actual);
    if (FAILED(hr)) return hr;
    return actual.cbBuffer < pRequest->cbBuffer ? E_FAIL : S_OK;
}

HRESULT CVCamStream::OnThreadCreate() {
    next_tick_ = 0;
    frame_time_ = 0;
    first_sample_ = true;
    timeBeginPeriod(1);  // 1 ms Sleep granularity for even frame pacing while streaming
    return S_OK;
}

HRESULT CVCamStream::OnThreadDestroy() {
    timeEndPeriod(1);
    return S_OK;
}

HRESULT CVCamStream::FillBuffer(IMediaSample* pms) {
    CheckPointer(pms, E_POINTER);
    REFERENCE_TIME avg;
    int w, h;
    OutFormat fmt;
    {
        CAutoLock lock(m_pFilter->pStateLock());
        const auto* pvi = reinterpret_cast<const VIDEOINFOHEADER*>(m_mt.Format());
        avg = pvi->AvgTimePerFrame ? pvi->AvgTimePerFrame : kDefaultFrameTime;
        w = pvi->bmiHeader.biWidth;
        h = pvi->bmiHeader.biHeight;
        fmt = OutFormatOf(*m_mt.Subtype());
    }

    // Pace the stream ourselves: a live source delivers at the negotiated rate.
    LONGLONG now = Now100ns();
    if (next_tick_ == 0) next_tick_ = now;
    if (next_tick_ > now) {
        Sleep(static_cast<DWORD>((next_tick_ - now) / 10000));
        now = Now100ns();
    }
    next_tick_ += avg;
    if (now - next_tick_ > 3 * avg) next_tick_ = now + avg;  // fell behind: do not burst

    BYTE* data = nullptr;
    HRESULT hr = pms->GetPointer(&data);
    if (FAILED(hr)) return hr;
    const long need = static_cast<long>(FrameBytes(w, h, fmt));
    if (pms->GetSize() < need) return E_FAIL;

    if (source_.Fetch())
        RenderI420(source_.I420(), source_.Width(), source_.Height(), data, w, h, fmt);
    else
        no_signal_.Render(data, w, h, fmt);
    pms->SetActualDataLength(need);

    // Timestamps in stream time when running with a clock, else frame counter.
    REFERENCE_TIME start, stop;
    IReferenceClock* clock = parent_->Clock();
    REFERENCE_TIME clock_now = 0;
    if (clock && SUCCEEDED(clock->GetTime(&clock_now))) {
        start = clock_now - parent_->StreamStart();
        if (start < 0) start = 0;
    } else {
        start = frame_time_;
    }
    stop = start + avg;
    frame_time_ = stop;
    pms->SetTime(&start, &stop);
    pms->SetSyncPoint(TRUE);
    pms->SetDiscontinuity(first_sample_ ? TRUE : FALSE);
    first_sample_ = false;
    return S_OK;
}

// ---------------------------------------------------------------- IAMStreamConfig

STDMETHODIMP CVCamStream::SetFormat(AM_MEDIA_TYPE* pmt) {
    CheckPointer(pmt, E_POINTER);
    CMediaType mt(*pmt);
    HRESULT hr = CheckMediaType(&mt);
    if (FAILED(hr)) return hr;
    {
        CAutoLock lock(m_pFilter->pStateLock());
        if (m_pFilter->IsActive()) return VFW_E_WRONG_STATE;
    }
    hr = SetMediaType(&mt);
    if (FAILED(hr)) return hr;
    {
        CAutoLock lock(m_pFilter->pStateLock());
        format_set_ = true;
    }
    IPin* peer = nullptr;
    if (SUCCEEDED(ConnectedTo(&peer)) && peer) {
        peer->Release();
        if (IFilterGraph* graph = parent_->Graph()) graph->Reconnect(this);
    }
    return S_OK;
}

STDMETHODIMP CVCamStream::GetFormat(AM_MEDIA_TYPE** ppmt) {
    CheckPointer(ppmt, E_POINTER);
    CAutoLock lock(m_pFilter->pStateLock());
    *ppmt = CreateMediaType(&m_mt);
    return *ppmt ? S_OK : E_OUTOFMEMORY;
}

STDMETHODIMP CVCamStream::GetNumberOfCapabilities(int* piCount, int* piSize) {
    CheckPointer(piCount, E_POINTER);
    CheckPointer(piSize, E_POINTER);
    *piCount = kNumFormats;
    *piSize = sizeof(VIDEO_STREAM_CONFIG_CAPS);
    return S_OK;
}

STDMETHODIMP CVCamStream::GetStreamCaps(int iIndex, AM_MEDIA_TYPE** ppmt, BYTE* pSCC) {
    CheckPointer(ppmt, E_POINTER);
    CheckPointer(pSCC, E_POINTER);
    if (iIndex < 0 || iIndex >= kNumFormats) return S_FALSE;
    CMediaType mt;
    HRESULT hr = BuildMediaType(iIndex, &mt, kDefaultFrameTime);
    if (FAILED(hr)) return hr;
    *ppmt = CreateMediaType(&mt);
    if (!*ppmt) return E_OUTOFMEMORY;

    const Size s = kSizes[iIndex % kNumSizes];
    auto* caps = reinterpret_cast<VIDEO_STREAM_CONFIG_CAPS*>(pSCC);
    ZeroMemory(caps, sizeof(VIDEO_STREAM_CONFIG_CAPS));
    caps->guid = FORMAT_VideoInfo;
    caps->VideoStandard = AnalogVideo_None;
    caps->InputSize = {s.w, s.h};
    caps->MinCroppingSize = caps->MaxCroppingSize = caps->InputSize;
    caps->CropGranularityX = caps->CropGranularityY = 1;
    caps->CropAlignX = caps->CropAlignY = 1;
    caps->MinOutputSize = caps->MaxOutputSize = caps->InputSize;
    caps->OutputGranularityX = caps->OutputGranularityY = 0;
    caps->MinFrameInterval = kMinFrameTime;
    caps->MaxFrameInterval = kMaxFrameTime;
    const LONG bits = static_cast<LONG>(FrameBytes(s.w, s.h, OutFormatOf(SubtypeAt(iIndex))) * 8);
    caps->MinBitsPerSecond = static_cast<LONG>(static_cast<LONGLONG>(bits) * UNITS / kMaxFrameTime);
    caps->MaxBitsPerSecond = static_cast<LONG>(static_cast<LONGLONG>(bits) * UNITS / kMinFrameTime);
    return S_OK;
}

// ---------------------------------------------------------------- IKsPropertySet

STDMETHODIMP CVCamStream::Get(REFGUID guidPropSet, DWORD dwPropID, LPVOID, DWORD, LPVOID pPropData, DWORD cbPropData,
                              DWORD* pcbReturned) {
    if (guidPropSet != AMPROPSETID_Pin) return E_PROP_SET_UNSUPPORTED;
    if (dwPropID != AMPROPERTY_PIN_CATEGORY) return E_PROP_ID_UNSUPPORTED;
    if (pPropData == nullptr && pcbReturned == nullptr) return E_POINTER;
    if (pcbReturned) *pcbReturned = sizeof(GUID);
    if (pPropData == nullptr) return S_OK;
    if (cbPropData < sizeof(GUID)) return E_UNEXPECTED;
    *static_cast<GUID*>(pPropData) = PIN_CATEGORY_CAPTURE;
    return S_OK;
}

STDMETHODIMP CVCamStream::QuerySupported(REFGUID guidPropSet, DWORD dwPropID, DWORD* pTypeSupport) {
    if (guidPropSet != AMPROPSETID_Pin) return E_PROP_SET_UNSUPPORTED;
    if (dwPropID != AMPROPERTY_PIN_CATEGORY) return E_PROP_ID_UNSUPPORTED;
    if (pTypeSupport) *pTypeSupport = KSPROPERTY_SUPPORT_GET;
    return S_OK;
}
