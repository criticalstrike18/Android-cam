# Quick Start Guide

Get up and running with Android-cam in under 2 minutes.

---

## ⚡ Fast Setup (USB Connection)

### 1. Prepare Android Device
1. On your phone, go to **Settings → About Phone**.
2. Tap **Build Number** 7 times to unlock **Developer Options**.
3. Go to **Settings → Developer Options** and enable **USB Debugging**.
4. Connect the phone to your PC using a USB-C cable.

### 2. Install & Start Mobile App (`AWA`)
Run the following PowerShell command in the project root:
```powershell
# Install the APK
adb install -r AWA\AWA-app-debug.apk

# Forward ports over USB
adb forward tcp:8080 tcp:8080
adb forward tcp:8554 tcp:8554

# Launch the app
adb shell am start -n com.sjbtechnologies.awa/.MainActivity
```

### 3. Launch Desktop Client (`AWC-GUI`)
```powershell
.\CLIENT\awc-gui\target\release\awc-gui.exe
```

The desktop app will automatically connect to the USB stream over `127.0.0.1:8554` (RTSP) and activate the virtual camera at 30 FPS.

---

## 📶 Wi-Fi Setup

1. Connect both PC and Android device to the same Wi-Fi router (5 GHz recommended).
2. Open `AWA` on your phone and note the local IP displayed on screen (e.g. `192.168.1.105`).
3. Open `AWC-GUI` on your PC, select **📶 Wi-Fi (IP)**, enter the IP, and click **Connect**.

---

## 🎥 Select Webcam in Your Apps

Open your video calling software (Zoom, Google Meet, Discord, Microsoft Teams, OBS) and choose **OBS Virtual Camera** as your webcam device.
