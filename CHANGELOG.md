# Changelog

What changed in each Remote Duo Host release. The host updates itself: it
checks when it starts and every 6 hours, and offers the update in its menu.

## 0.3.1

- **Receive files from Remote Duo Desktop:** drop files on the remote screen
  in the Mac, Windows or Linux desktop app and they're saved in this
  computer's Downloads folder. Names are kept (a taken name gets a number),
  and a transfer that stops partway leaves nothing behind.
- **Windows: the screensaver no longer locks out your phone.** Moving the
  mouse or typing from your phone wakes it, as it would at the PC. Before,
  screen view kept working but input was ignored until the screensaver was
  dismissed at the PC.
- **Two devices at once:** a phone and a desktop can both be connected to the
  same computer, so leaving your desktop app connected at home doesn't stop
  your phone from connecting when you're out. A third gets a message saying
  two are already connected. A device that reconnects (after its Wi-Fi
  dropped, say) takes over its own earlier connection instead of counting
  twice, and connections that go silent for 20 seconds are closed.

## 0.3.0

- **Windows: sign in after a restart.** With "Start Automatically" on, your
  phone can reach the PC's sign-in screen right after it restarts, before
  anyone has signed in, using the phone you already paired. Once you sign in,
  the app on the PC takes over and the phone reconnects to it by itself
  (with Remote Duo for iPhone 1.0.1; on 1.0, tap Retry).
  Pairing a new phone still needs the PIN from the app's menu, after signing
  in.
- **"Start at Login" is now "Start Automatically"**, since on Windows it now
  also covers the time before anyone signs in. It's still off until you turn
  it on.
- **Windows:** screen view opens straight to the sign-in screen when the PC is
  locked, instead of switching to it a moment later.

## 0.2.1

- **Mac: the app no longer quits on launch if it isn't allowed to control
  the computer yet.** macOS can ask again for Accessibility permission after
  an update. Before, the app closed right away without saying why. Now it
  opens, its menu shows "Allow Accessibility…", and the trackpad starts
  working as soon as you allow it.
- **Mac and Linux: the app reopens after an update.** It used to try
  reopening before the old copy had finished closing, which sometimes left
  nothing running.

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
