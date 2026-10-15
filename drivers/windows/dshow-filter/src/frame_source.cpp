#include "frame_source.h"

#include <algorithm>
#include <cstring>

#include "../../common/lb_vcam_shm.h"

namespace {

inline uint8_t Clamp8(int v) { return static_cast<uint8_t>(v < 0 ? 0 : (v > 255 ? 255 : v)); }

// BT.709 limited range, same coefficients as the Rust preview.
inline void YuvToRgb(int y, int u, int v, uint8_t& r, uint8_t& g, uint8_t& b) {
    const int c = 298 * (y - 16), d = u - 128, e = v - 128;
    r = Clamp8((c + 459 * e + 128) >> 8);
    g = Clamp8((c - 55 * d - 136 * e + 128) >> 8);
    b = Clamp8((c + 541 * d + 128) >> 8);
}

inline void RgbToYuv(int r, int g, int b, uint8_t& y, uint8_t& u, uint8_t& v) {
    y = Clamp8(16 + ((47 * r + 157 * g + 16 * b + 128) >> 8));
    u = Clamp8(128 + ((-26 * r - 87 * g + 112 * b + 128) >> 8));
    v = Clamp8(128 + ((112 * r - 102 * g - 10 * b + 128) >> 8));
}

void FillBlack(BYTE* dst, int dw, int dh, OutFormat fmt) {
    if (fmt == OutFormat::RGB24) {
        std::memset(dst, 0, FrameBytes(dw, dh, fmt));
        return;
    }
    uint32_t* p = reinterpret_cast<uint32_t*>(dst);
    const uint32_t black = 16u | (128u << 8) | (16u << 16) | (128u << 24);  // Y U Y V
    std::fill(p, p + (static_cast<size_t>(dw) * dh) / 2, black);
}

}  // namespace

size_t FrameBytes(int w, int h, OutFormat fmt) {
    if (fmt == OutFormat::YUY2) return static_cast<size_t>(w) * h * 2;
    const size_t stride = (static_cast<size_t>(w) * 3 + 3) & ~static_cast<size_t>(3);
    return stride * h;
}

FrameSource::~FrameSource() { Close(); }

void FrameSource::Close() {
    if (view_) UnmapViewOfFile(view_);
    if (map_) CloseHandle(map_);
    view_ = nullptr;
    map_ = nullptr;
}

bool FrameSource::Open() {
    if (view_) return true;
    const ULONGLONG now = GetTickCount64();
    if (last_open_attempt_ != 0 && now - last_open_attempt_ < 1000) return false;
    last_open_attempt_ = now;
    map_ = OpenFileMappingW(FILE_MAP_READ, FALSE, LB_VCAM_SHM_NAME);
    if (!map_) return false;
    view_ = static_cast<const uint8_t*>(MapViewOfFile(map_, FILE_MAP_READ, 0, 0, LB_VCAM_TOTAL_SIZE));
    if (!view_) {
        CloseHandle(map_);
        map_ = nullptr;
        return false;
    }
    return true;
}

bool FrameSource::Fetch() {
    if (!Open()) return false;
    const auto* hdr = reinterpret_cast<const LbVcamHeader*>(view_);
    if (hdr->magic != LB_VCAM_MAGIC || hdr->version != LB_VCAM_VERSION) return false;
    if (GetTickCount64() - hdr->heartbeat_ms > LB_VCAM_STALE_MS) {
        have_frame_ = false;
        return false;
    }
    for (int attempt = 0; attempt < 4; ++attempt) {
        const uint32_t slot = hdr->latest_slot & 1u;
        const uint8_t* base = view_ + LB_VCAM_HEADER_SIZE + static_cast<size_t>(slot) * LB_VCAM_SLOT_SIZE;
        const auto* s = reinterpret_cast<const LbVcamSlot*>(base);
        const uint64_t seq1 = s->seq;
        MemoryBarrier();
        if (seq1 & 1u) {
            YieldProcessor();
            continue;
        }
        const uint32_t w = s->width, h = s->height;
        if (w < 2 || h < 2 || w > LB_VCAM_MAX_WIDTH || h > LB_VCAM_MAX_HEIGHT) return have_frame_;
        const size_t n = static_cast<size_t>(w) * h + 2 * static_cast<size_t>((w + 1) / 2) * ((h + 1) / 2);
        buf_.resize(n);
        std::memcpy(buf_.data(), base + LB_VCAM_SLOT_HEADER_SIZE, n);
        MemoryBarrier();
        if (s->seq == seq1) {
            width_ = static_cast<int>(w);
            height_ = static_cast<int>(h);
            have_frame_ = true;
            return true;
        }
    }
    // Writer kept overwriting while we copied: reuse the previous frame.
    return have_frame_;
}

void RenderI420(const uint8_t* src, int sw, int sh, BYTE* dst, int dw, int dh, OutFormat fmt) {
    FillBlack(dst, dw, dh, fmt);
    // Fit with letterbox / pillarbox; keep offsets and sizes even for YUY2 pairs.
    int tw = dw, th = static_cast<int>(static_cast<int64_t>(dw) * sh / sw);
    if (th > dh) {
        th = dh;
        tw = static_cast<int>(static_cast<int64_t>(dh) * sw / sh);
    }
    tw &= ~1;
    th &= ~1;
    if (tw < 2 || th < 2) return;
    const int ox = ((dw - tw) / 2) & ~1;
    const int oy = (dh - th) / 2;

    const int cw = (sw + 1) / 2;
    const int ch = (sh + 1) / 2;
    const uint8_t* yp = src;
    const uint8_t* up = src + static_cast<size_t>(sw) * sh;
    const uint8_t* vp = up + static_cast<size_t>(cw) * ch;

    std::vector<int> xmap(tw);
    for (int x = 0; x < tw; ++x) xmap[x] = static_cast<int>(static_cast<int64_t>(x) * sw / tw);

    if (fmt == OutFormat::YUY2) {
        const size_t stride = static_cast<size_t>(dw) * 2;
        for (int y = 0; y < th; ++y) {
            const int sy = static_cast<int>(static_cast<int64_t>(y) * sh / th);
            const uint8_t* yrow = yp + static_cast<size_t>(sy) * sw;
            const uint8_t* urow = up + static_cast<size_t>(sy / 2) * cw;
            const uint8_t* vrow = vp + static_cast<size_t>(sy / 2) * cw;
            BYTE* out = dst + (oy + y) * stride + static_cast<size_t>(ox) * 2;
            for (int x = 0; x < tw; x += 2) {
                const int sx0 = xmap[x], sx1 = xmap[x + 1];
                out[0] = yrow[sx0];
                out[1] = urow[sx0 / 2];
                out[2] = yrow[sx1];
                out[3] = vrow[sx0 / 2];
                out += 4;
            }
        }
    } else {
        const size_t stride = FrameBytes(dw, 1, OutFormat::RGB24);
        for (int y = 0; y < th; ++y) {
            const int sy = static_cast<int>(static_cast<int64_t>(y) * sh / th);
            const uint8_t* yrow = yp + static_cast<size_t>(sy) * sw;
            const uint8_t* urow = up + static_cast<size_t>(sy / 2) * cw;
            const uint8_t* vrow = vp + static_cast<size_t>(sy / 2) * cw;
            // RGB24 DIBs with positive height are bottom-up.
            BYTE* out = dst + static_cast<size_t>(dh - 1 - (oy + y)) * stride + static_cast<size_t>(ox) * 3;
            for (int x = 0; x < tw; ++x) {
                const int sx = xmap[x];
                uint8_t r, g, b;
                YuvToRgb(yrow[sx], urow[sx / 2], vrow[sx / 2], r, g, b);
                out[0] = b;
                out[1] = g;
                out[2] = r;
                out += 3;
            }
        }
    }
}

void NoSignalCard::Render(BYTE* dst, int dw, int dh, OutFormat fmt) {
    const size_t bytes = FrameBytes(dw, dh, fmt);
    if (cache_.size() == bytes && w_ == dw && h_ == dh && fmt_ == fmt) {
        std::memcpy(dst, cache_.data(), bytes);
        return;
    }

    BITMAPINFO bi = {};
    bi.bmiHeader.biSize = sizeof(BITMAPINFOHEADER);
    bi.bmiHeader.biWidth = dw;
    bi.bmiHeader.biHeight = -dh;  // top-down
    bi.bmiHeader.biPlanes = 1;
    bi.bmiHeader.biBitCount = 32;
    bi.bmiHeader.biCompression = BI_RGB;
    void* bits = nullptr;
    HDC dc = CreateCompatibleDC(nullptr);
    HBITMAP bmp = dc ? CreateDIBSection(dc, &bi, DIB_RGB_COLORS, &bits, nullptr, 0) : nullptr;
    if (!bmp || !bits) {
        if (dc) DeleteDC(dc);
        FillBlack(dst, dw, dh, fmt);
        return;
    }
    HGDIOBJ old_bmp = SelectObject(dc, bmp);
    RECT all = {0, 0, dw, dh};
    HBRUSH bg = CreateSolidBrush(RGB(28, 30, 38));
    FillRect(dc, &all, bg);
    DeleteObject(bg);
    SetBkMode(dc, TRANSPARENT);

    HFONT big = CreateFontW(-(dh / 12), 0, 0, 0, FW_SEMIBOLD, FALSE, FALSE, FALSE, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS,
                            CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, DEFAULT_PITCH, L"Segoe UI");
    HFONT small = CreateFontW(-(dh / 24), 0, 0, 0, FW_NORMAL, FALSE, FALSE, FALSE, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS,
                              CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, DEFAULT_PITCH, L"Segoe UI");
    HGDIOBJ old_font = SelectObject(dc, big);
    SetTextColor(dc, RGB(235, 235, 240));
    RECT r1 = {0, dh / 2 - dh / 8, dw, dh / 2};
    DrawTextW(dc, L"Lensbridge Camera", -1, &r1, DT_CENTER | DT_SINGLELINE | DT_BOTTOM);
    SelectObject(dc, small);
    SetTextColor(dc, RGB(160, 165, 180));
    RECT r2 = {dw / 16, dh / 2 + dh / 40, dw - dw / 16, dh};
    DrawTextW(dc, L"Нет сигнала: запустите Lensbridge на ПК и на телефоне", -1, &r2, DT_CENTER | DT_WORDBREAK | DT_TOP);
    GdiFlush();

    cache_.resize(bytes);
    const auto* px = static_cast<const uint8_t*>(bits);  // BGRA, top-down
    if (fmt == OutFormat::YUY2) {
        for (int y = 0; y < dh; ++y) {
            const uint8_t* row = px + static_cast<size_t>(y) * dw * 4;
            uint8_t* out = cache_.data() + static_cast<size_t>(y) * dw * 2;
            for (int x = 0; x + 1 < dw; x += 2) {
                uint8_t y0, u0, v0, y1, u1, v1;
                RgbToYuv(row[x * 4 + 2], row[x * 4 + 1], row[x * 4], y0, u0, v0);
                RgbToYuv(row[x * 4 + 6], row[x * 4 + 5], row[x * 4 + 4], y1, u1, v1);
                out[x * 2] = y0;
                out[x * 2 + 1] = static_cast<uint8_t>((u0 + u1) / 2);
                out[x * 2 + 2] = y1;
                out[x * 2 + 3] = static_cast<uint8_t>((v0 + v1) / 2);
            }
        }
    } else {
        const size_t stride = FrameBytes(dw, 1, OutFormat::RGB24);
        std::fill(cache_.begin(), cache_.end(), 0);
        for (int y = 0; y < dh; ++y) {
            const uint8_t* row = px + static_cast<size_t>(y) * dw * 4;
            uint8_t* out = cache_.data() + static_cast<size_t>(dh - 1 - y) * stride;
            for (int x = 0; x < dw; ++x) {
                out[x * 3] = row[x * 4];
                out[x * 3 + 1] = row[x * 4 + 1];
                out[x * 3 + 2] = row[x * 4 + 2];
            }
        }
    }

    SelectObject(dc, old_font);
    SelectObject(dc, old_bmp);
    DeleteObject(big);
    DeleteObject(small);
    DeleteObject(bmp);
    DeleteDC(dc);

    w_ = dw;
    h_ = dh;
    fmt_ = fmt;
    std::memcpy(dst, cache_.data(), bytes);
}
