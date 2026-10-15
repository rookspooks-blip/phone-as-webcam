// "Lensbridge Camera": a DirectShow video capture source filter that shows the
// frames published by the Lensbridge desktop app.
#pragma once

#include <streams.h>

#include "frame_source.h"

// {CB4E2A03-21DA-4355-8213-E8E2E58EAF82}, defined in dll.cpp.
extern "C" const GUID CLSID_LensbridgeVCam;

#define LB_VCAM_FRIENDLY_NAME L"Lensbridge Camera"

class CVCamStream;

class CVCam : public CSource, public IAMFilterMiscFlags {
public:
    static CUnknown* WINAPI CreateInstance(LPUNKNOWN lpunk, HRESULT* phr);

    DECLARE_IUNKNOWN;
    STDMETHODIMP NonDelegatingQueryInterface(REFIID riid, void** ppv) override;

    // IAMFilterMiscFlags
    STDMETHODIMP_(ULONG) GetMiscFlags() override { return AM_FILTER_MISC_FLAGS_IS_SOURCE; }

    IFilterGraph* Graph() const { return m_pGraph; }
    IReferenceClock* Clock() const { return m_pClock; }
    REFERENCE_TIME StreamStart() const { return m_tStart; }

private:
    CVCam(LPUNKNOWN lpunk, HRESULT* phr);
    CVCamStream* Stream() const;
};

class CVCamStream : public CSourceStream, public IAMStreamConfig, public IKsPropertySet {
public:
    CVCamStream(HRESULT* phr, CVCam* parent, LPCWSTR pin_name);

    DECLARE_IUNKNOWN;
    STDMETHODIMP NonDelegatingQueryInterface(REFIID riid, void** ppv) override;

    // IQualityControl (we are a live source: ignore quality messages)
    STDMETHODIMP Notify(IBaseFilter*, Quality) override { return E_NOTIMPL; }

    // IAMStreamConfig
    STDMETHODIMP SetFormat(AM_MEDIA_TYPE* pmt) override;
    STDMETHODIMP GetFormat(AM_MEDIA_TYPE** ppmt) override;
    STDMETHODIMP GetNumberOfCapabilities(int* piCount, int* piSize) override;
    STDMETHODIMP GetStreamCaps(int iIndex, AM_MEDIA_TYPE** ppmt, BYTE* pSCC) override;

    // IKsPropertySet
    STDMETHODIMP Set(REFGUID, DWORD, LPVOID, DWORD, LPVOID, DWORD) override { return E_NOTIMPL; }
    STDMETHODIMP Get(REFGUID guidPropSet, DWORD dwPropID, LPVOID pInstanceData, DWORD cbInstanceData, LPVOID pPropData,
                     DWORD cbPropData, DWORD* pcbReturned) override;
    STDMETHODIMP QuerySupported(REFGUID guidPropSet, DWORD dwPropID, DWORD* pTypeSupport) override;

protected:
    // CSourceStream
    HRESULT FillBuffer(IMediaSample* pms) override;
    HRESULT DecideBufferSize(IMemAllocator* pAlloc, ALLOCATOR_PROPERTIES* pRequest) override;
    HRESULT CheckMediaType(const CMediaType* pMediaType) override;
    HRESULT GetMediaType(int iPosition, CMediaType* pmt) override;
    HRESULT SetMediaType(const CMediaType* pmt) override;
    HRESULT OnThreadCreate() override;

private:
    CVCam* parent_;
    FrameSource source_;
    NoSignalCard no_signal_;
    LONGLONG next_tick_ = 0;       // QPC-based pacing, 100 ns units
    REFERENCE_TIME frame_time_ = 0;  // fallback timestamps without a clock
    bool first_sample_ = true;
};
