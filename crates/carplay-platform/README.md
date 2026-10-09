# carplay-platform

Read-only diagnostics and native transport boundaries for Windows and Linux.
macOS is a GUI/core build preview; native receiver connection adapters are not
implemented there.

- `collect_diagnostics()` produces serializable network, Classic Bluetooth,
  Apple USB and GStreamer capability observations. It omits USB serial numbers,
  device/adapter names, Bluetooth addresses, SSIDs, passwords and raw OS messages.
  IP addresses and interface indices are deliberately included for IPv6 routing.
- `RfcommStream::connect(ConnectOptions)` connects to an already paired peer by
  UUID (Windows SDP / Linux BlueZ libbluetooth), or by an explicitly supplied
  RFCOMM channel. `RfcommListener::bind` listens on a known local channel.
- `bluetooth::paired_devices()` reads Windows' remembered/authenticated Classic
  Bluetooth cache without issuing an inquiry. `bluetooth::local_adapters()` reads
  radio names and addresses. These picker-only records are not serializable and
  redact their Debug representation; `address.to_colon_string()` explicitly
  reveals an address for local UI display. They never enter host diagnostics.
  Linux currently returns an explicit unsupported error for these picker APIs.
  A remembered device is not proof that it is reachable or offers CarPlay.
- `wifi::current_wifi()` reads the connected Windows WLAN SSID, negotiated
  authentication mode, and interface index for local UI autofill. It never scans,
  reads a password/profile, or changes the network. Its Debug output redacts the
  SSID and it is not serializable. `current_wifi_on(Some(index))` selects a
  particular Wi-Fi adapter. Windows privacy/access errors remain explicit so
  the UI can accept manual input. Negotiated SAE does not distinguish a WPA3
  transition-capable AP from a WPA3-only AP; don't guess that wire mode.
- **Listening does not register an SDP service.** Service publication,
  discoverability, pairing and CarPlay's iAP2 handshake belong to higher layers.
  Linux UUID lookup requires `libbluetooth.so.3`; its synchronous SDP call has
  BlueZ's timeout, separate from the socket connect timeout. Avoid it on UI threads.
- USB configuration parsing distinguishes descriptor indices from configuration
  values, finds USBMUX + CDC NCM and honours CDC Union interface associations.
  WinUSB cannot select a different configuration, but the backend accepts a
  non-first configuration already activated by usbccgp. Explicit administrator
  preparation selects the descriptor-proven configuration for one phone and
  preserves the original registry values for restoration.
- USB diagnostics enumerate and read descriptors through nusb. They never claim
  an interface, detach/replace drivers, reset devices or change configurations.
  Permission-denied descriptor reads remain visible in the report.

`cargo run -p carplay-platform --example diagnose` runs the native probes.
`cargo run -p carplay-platform --example bluetooth_cache` checks the two Windows
picker APIs and prints only adapter/device counts.
`cargo test -p carplay-platform` exercises descriptor bytes, bounds, address
parsing, scope classification and redaction. Test descriptors are reconstructed
protocol fixtures, not hardware captures. Real paired-device/USB transfer tests
require attached hardware and are not represented as passing by these tests.


Connection modes now also use explicitly requested platform operations:
- `wifi::credentials_for_address` reads only the selected active Wi-Fi profile
  into zeroizing memory; identity-only enumeration never accesses the key.
- `hotspot::HotspotSession` owns a dedicated Windows WinRT / Linux NetworkManager
  actor. Existing hotspots are reused, owned hotspots are stopped, and temporary
  configuration is restored or removed. Errors are redacted and cleanup failures
  are returned to the caller.
- `usb::native` provides explicit mode/configuration/bulk operations.
  `usb::system` opens the selected device's system CarKit service and scoped
  CDC-NCM interface on Linux and Windows. Windows uses Apple Mobile Device
  Service, a separately prepared libimobiledevice runtime and the system UsbNcm
  driver; an optional parent filter only sends the mode-switch control request.
  One Windows/iPhone combination has passed video, touch, audio, pause and
  reconnect checks. Linux physical-device acceptance, long-running stability,
  wider device compatibility and complete driver restoration remain unverified.
  See [USB prerequisites](../../docs/USB.md) and
  [Windows preparation/recovery](../../docs/WINDOWS_USB_DRIVER.md).
