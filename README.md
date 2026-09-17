# Android-cam — High-Performance Android Webcam System

Turn your Android smartphone into a broadcast-grade, ultra-low latency PC webcam with a fully local, zero-cloud architecture.

Open-source (GPL-3.0)

---

## 📦 Architecture Overview

The system consists of two tightly coupled, high-performance components:

- 📱 **AWA (Android Webcam App)**: Native Kotlin Android application using Camera2 and MediaCodec hardware acceleration. Streams sub-second H.264 over RTSP (port 8554) with a secondary low-latency MJPEG HTTP fallback (port 8080).
- 💻 **AWC-GUI (Pure Rust Desktop Client)**: Ultra-fast native client written in **Rust (`egui` / `eframe`)** featuring a modern **Shadcn-inspired Dark Theme**, fully responsive maximized video viewport, integrated OpenH264 decoder, and silent direct memory output to **OBS Virtual Camera** (DirectShow).

---

## 🚀 Key Features

### Desktop Client (`AWC-GUI`)
- **Shadcn-Inspired Dark Design**: Sleek zinc palette (`#09090b` / `#18181b` / `#27272a`), refined cards, segmented controls, and pulsing status badge pills.
- **Fully Responsive Dynamic Viewport**: The video preview automatically expands to fill 100% of available window space while strictly preserving aspect ratio. Resizable right-hand controls sidebar with smooth vertical scrolling.
- **Hardware-Accelerated Zero-Lag Decoding**: Direct in-memory OpenH264 decoding pushing NV12 frames directly into the DirectShow virtual camera buffer at **30 FPS with sub-second latency**.
- **Silent Virtual Camera Integration**: Seamlessly bundled OBS Virtual Camera output without any intrusive command prompts or extra buttons. Compatible with **Zoom, Microsoft Teams, Google Meet, Discord, OBS Studio, Skype**, and web browsers.
- **Lock-Free Telemetry**: Real-time HUD overlay on the video feed showing resolution, virtual camera status, and measured FPS.

### Mobile Application (`AWA`)
- **Thermal & Battery Optimization**: Native offscreen EGL rendering automatically detaches physical display composition when the preview dims, idling CPU at **~0%** and keeping phone thermals cool (36°C).
- **Auto-Rotation & Orientation Lock**: Physical device rotation handled smoothly inside the GPU shader matrix without activity recreation or stream interruptions.
- **Hardware Controls**:
  - Rear and Front camera switching
  - Continuous digital zoom (clamped 1.0x to 5.0x)
  - Exposure compensation index adjustment
  - Flashlight / Torch toggle
  - Touch-to-focus and autofocus management
- **Serialized State Engine**: Coroutine mutex synchronization prevents pipeline clashes even under rapid control triggers.

---

## 🛠️ Requirements & Prerequisites

### PC (Desktop Client)
- **OS**: Windows 10 or Windows 11 (64-bit)
- **Rust Toolchain**: `cargo` & `rustc` (optional, only if building from source)
- **Virtual Camera**: OBS Virtual Camera driver (registered automatically)

### Android Device (Mobile App)
- **OS**: Android 8.0 (API 26) or higher
- **Camera Permission**: Required for capturing camera video

---

## 🏃 Running & Quick Start

### 1. Launch Desktop Client (`awc-gui`)

#### Run Prebuilt Release Binary:
```powershell
.\CLIENT\awc-gui\target\release\awc-gui.exe
```

#### Or Build from Source:
```powershell
cd CLIENT\awc-gui
cargo build --release
```
The optimized executable will be generated at `CLIENT/awc-gui/target/release/awc-gui.exe`.

---

### 2. Install Mobile App (`AWA`)

Install the prebuilt debug APK directly using ADB:
```powershell
adb install -r AWA\AWA-app-debug.apk
adb shell am start -n com.sjbtechnologies.awa/.MainActivity
```
Or open the `AWA/` project directory in **Android Studio** and click **Run**.

---

### 3. Connect Phone & PC

#### Option A: USB Cable (Recommended for Lowest Latency)
1. Enable **Developer Options** and **USB Debugging** on your phone.
2. Connect your phone to your PC with a USB cable.
3. In `awc-gui`, select **🔌 USB (ADB)** mode and click **⚡ Forward ADB Ports**.
4. The client will automatically connect to `127.0.0.1` and start streaming video instantly.

#### Option B: Wi-Fi (Wireless Freedom)
1. Ensure your PC and phone are connected to the same Wi-Fi network (5 GHz recommended).
2. Enter your phone's local IP address (displayed in the mobile app, e.g. `192.168.1.50`) into `awc-gui`.
3. Click **Connect**.

---

## 🎥 Using as a Virtual Camera in Video Apps

Once `awc-gui` is running, the virtual camera is immediately available system-wide:

1. Open **Discord**, **Zoom**, **Google Meet**, **Microsoft Teams**, or **OBS Studio**.
2. Go to **Settings → Video / Camera**.
3. Select **OBS Virtual Camera** as your input device.
4. Enjoy smooth 1080p/720p 30 FPS video streaming with sub-second latency!

---

## 📄 License

GPL-3.0 License.
