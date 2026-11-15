# Changelog

What changed in each Remote Duo Host release. The host updates itself: it
checks when it starts and every 6 hours, and offers the update in its menu.

## 0.1.3

- **Windows:** screen view comes back by itself after you sign in at the lock
  screen. Before, it stayed black until you disconnected and reconnected.

## 0.1.2

- **Windows:** screen view works again after you close and reopen it, for
  example when you rotate the phone to landscape and back. Before, the second
  time failed with "The parameter is incorrect (0x80070057)" until the host was
  restarted. Screen capture now uses Windows Graphics Capture.

## 0.1.1

- **Windows:** phones can connect on networks Windows calls Public, which is
  how Windows 11 marks most new Wi-Fi. Before, the phone could see the PC but
  couldn't connect unless the network was set to Private.
- **Security:** after 5 wrong PINs in a row, pairing pauses for a minute, so
  nobody on a shared network can guess their way in. Phones that already
  paired aren't affected.
- **Windows and Linux:** the tray icon is purple, so it shows on light
  taskbars as well as dark ones.
- **Windows:** the PowerShell installer no longer sits on "Installing" after
  the install has finished.

## 0.1.0

First release: trackpad, keyboard, screen view, computer sound and media
remote for the Remote Duo iPhone app, on macOS, Windows and Linux.
