# Changelog

What changed in each Remote Duo Host release. The host updates itself: it
checks when it starts and every 6 hours, and offers the update in its menu.

## 0.2.0

- **Windows: use the sign-in screen from your phone.** Wake a locked PC, see
  the sign-in screen in screen view, move the cursor and type your password,
  with no keyboard or mouse at the PC. The installer adds a small service,
  "Remote Duo Sign-in Helper", which is the only part that can reach the
  sign-in screen. It works only while the PC is locked, so it can't be used
  to approve administrator (UAC) prompts. The cursor shows in screen view;
  on the PC's own display Windows keeps it hidden until its own mouse moves.
- **See which version you have:** the menu (menu bar on Mac, tray on Windows
  and Linux) shows "Version 0.2.0 · What's New…", which opens that release's
  notes. In a terminal, `remote-duo-host --version`.
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
