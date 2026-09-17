# High-Performance USB Cable Connection

Connecting over USB via ADB port forwarding provides optimal streaming performance with the lowest latency, zero network jitter, and no reliance on Wi-Fi bandwidth.

---

## Why Use USB Mode?

- **Sub-second Latency**: Hardware-encoded H.264 packets travel directly through the USB controller.
- **Zero Wi-Fi Congestion**: Avoids Wi-Fi packet drops, router congestion, and wireless interference.
- **Simultaneous Charging**: The USB connection keeps your phone powered during long video conferences or streams.

---

## Step-by-Step Instructions

### 1. Enable Developer Options & USB Debugging
1. Open **Settings → About Phone**.
2. Tap **Build Number** 7 times until you see the prompt *"You are now a developer!"*.
3. Go back to **Settings → System → Developer Options** (location may vary by manufacturer).
4. Turn on **USB Debugging**.

### 2. Connect USB Cable
1. Connect your phone to your PC via a USB data cable.
2. A prompt will appear on your phone screen: *"Allow USB debugging?"*. Check *"Always allow from this computer"* and tap **Allow**.

### 3. Forward ADB Ports
In the `AWC-GUI` desktop app:
1. Select **🔌 USB (ADB)** mode.
2. Click **⚡ Forward ADB Ports**.

Or manually run via PowerShell / Command Prompt:
```powershell
adb forward tcp:8080 tcp:8080
adb forward tcp:8554 tcp:8554
```

### 4. Automatic Reconnection & Failover
- In USB mode, `AWC-GUI` automatically communicates with `127.0.0.1:8080` (HTTP control) and `127.0.0.1:8554` (RTSP video).
- If the USB cable is accidentally unplugged, the client will smoothly fail over to the configured Wi-Fi IP address if **Auto Wi-Fi failover** is enabled.
