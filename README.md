# Android-cam — Android Webcam System

Turn your Android phone into a high-quality PC webcam using a fully local, serverless architecture.

Open-source (GPL-3.0)

---

## 📦 What's Included

This is a **complete professional solution** comprising:

- 📱 **AWA (Android Webcam App)**: Kotlin-based mobile app that captures camera input and acts as the local streaming server (MJPEG / RTSP).
- 💻 **AWC-GUI (Pure Rust Desktop Client)**: Ultra-fast, lightweight native client built with **`egui` / `eframe`** (no Node.js or web runtime required) with direct Windows DirectShow virtual webcam output.
- 💻 **AWC-Tauri (Alternative Client)**: Tauri v2 + React 19 web-styled client.
- 🔌 **USB Connection (ADB Forward)**: Low-latency, jitter-free, offline streaming.
- 📡 **WiFi Connection**: Wireless freedom across the local network.

---

## 🚀 Key Features

### Mobile App (`AWA`)
- **Resolutions**: 480p, 720p, 1080p, and up to 4K UHD.
- **Controls**: Front / back camera switching, torch/flash toggle, digital zoom, and exposure controls.
- **Orientation**: Auto-sensor rotation with manual override (0°, 90°, 180°, 270°).
- **Protocols**: MJPEG HTTP streaming and RTSP streaming.
- **Battery Friendly**: Optimized pipeline with low battery draw.

### Desktop Client (`AWC-GUI`)
- **Pure Rust Native App**: Immediate-mode UI via `egui` with zero Node.js/web dependencies.
- **Virtual Webcam Device**: Registers a native DirectShow camera via `softcam.dll` compatible with **Zoom, Microsoft Teams, Google Meet, Discord, OBS Studio, Skype**, etc.
- **Silent Background Subprocesses**: Background ADB port forwarding and FFmpeg RTSP ingestion run completely silently without annoying console/terminal popups.
- **Dual View**: Built-in desktop UI preview plus an embedded local HTTP dashboard (`http://127.0.0.1:8081`).

---

## 🛠️ Requirements & Prerequisites

### PC (Desktop Client)
- **OS**: Windows 10+ (64-bit)
- **Rust Toolchain**: `cargo` & `rustc` (if building from source)
- **Virtual Webcam Library**: `softcam.dll` (included in repository)
- **ADB** (optional for USB mode, included in client)

### Phone (Mobile App)
- **OS**: Android 8.0 (Oreo) or higher
- **Camera Permission**: Required for video streaming

---

## 🏃 Quick Start & Running

### 1. Run the Native Desktop Client (`awc-gui`)
No Node.js or npm needed!

#### Run the Prebuilt Binary:
```powershell
# Run the standalone executable:
.\CLIENT\awc-gui\target\release\awc-gui.exe
```
*(Ensure `softcam.dll` is located alongside `awc-gui.exe` so the virtual webcam registers automatically).*

#### Or Build from Source:
```powershell
cd CLIENT\awc-gui
cargo build --release
```
The compiled binary will be placed at `CLIENT/awc-gui/target/release/awc-gui.exe`.

---

### 2. Install the Android App (`AWA`)
- Install the prebuilt APK from [`AWA/AWA-Android.Webcam.App.V1.0.3.apk`](AWA/AWA-Android.Webcam.App.V1.0.3.apk) on your Android device.
- Or open the [`AWA`](AWA/) folder in **Android Studio** and click **Run**.

---

### 3. Connect Phone & PC

#### Option A: USB Connection (Recommended for Lowest Latency)
1. Enable **Developer Options** and **USB Debugging** on your phone.
2. Connect your phone to your PC via USB cable.
3. In `awc-gui`, click **Run ADB Forward** (sets up port forwards for `8080` and `8554`).
4. Set Phone IP to `127.0.0.1` and click **Connect**.

#### Option B: WiFi Connection
1. Ensure both PC and phone are on the same WiFi network (5 GHz recommended).
2. Note the IP displayed on your phone's screen (e.g. `192.168.1.50`).
3. Enter that IP into `awc-gui` and click **Connect**.

---

## 🎥 Using as Virtual Webcam in Video Apps

Once connected in `awc-gui`, your virtual camera device **"Softcam"** is active:
1. Open Zoom, OBS Studio, Discord, or Microsoft Teams.
2. Go to **Video / Camera Settings**.
3. Select **Softcam** as your video input device.

---

## 📁 Repository Structure

```
Android-cam/
├── AWA/                    # Android Mobile Application (Kotlin + Jetpack Compose)
│   ├── app/                # App source code (CameraX, VideoStreamServer)
│   └── *.apk               # Prebuilt APK binaries
├── CLIENT/
│   ├── awc-gui/            # Pure Rust + egui Desktop Client (recommended)
│   │   ├── src/main.rs     # Stream ingestion, UI, and DirectShow bridge
│   │   └── Cargo.toml
│   └── tauri-client/       # Tauri v2 + React 19 desktop client
├── assets/                 # Screenshots & visual guides
└── README.md
```

---

## 📄 License

This project is licensed under the **GNU General Public License v3.0 (GPL-3.0)**.
See the [LICENSE](LICENSE) file for complete details.
