// Reads frames published by the Lensbridge desktop app and renders them into
// DirectShow buffers (YUY2 or bottom-up RGB24), scaled with letterboxing.
#pragma once

#include <windows.h>

#include <cstdint>
#include <vector>

class FrameSource {
public:
    FrameSource() = default;
    ~FrameSource();
    FrameSource(const FrameSource&) = delete;
    FrameSource& operator=(const FrameSource&) = delete;

    // Copies the newest frame from shared memory. Returns false when the app
    // is not running or has not published a frame recently.
    bool Fetch();

    int Width() const { return width_; }
    int Height() const { return height_; }
    const uint8_t* I420() const { return buf_.data(); }

private:
    bool Open();
    void Close();

    HANDLE map_ = nullptr;
    const uint8_t* view_ = nullptr;
    ULONGLONG last_open_attempt_ = 0;
    std::vector<uint8_t> buf_;
    int width_ = 0;
    int height_ = 0;
    bool have_frame_ = false;
};

enum class OutFormat { YUY2, RGB24 };

// Scales an I420 picture into `dst` (dw x dh) keeping the aspect ratio.
void RenderI420(const uint8_t* src, int sw, int sh, BYTE* dst, int dw, int dh, OutFormat fmt);

// "No signal" card, rendered once per size/format and cached.
class NoSignalCard {
public:
    void Render(BYTE* dst, int dw, int dh, OutFormat fmt);

private:
    std::vector<uint8_t> cache_;
    int w_ = 0, h_ = 0;
    OutFormat fmt_ = OutFormat::YUY2;
};

size_t FrameBytes(int w, int h, OutFormat fmt);
