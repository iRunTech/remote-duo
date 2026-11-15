; Inno Setup script for RemoteDuoHost-Windows-Setup.exe.
; Built in CI:  iscc /DAppVersion=1.2.3 scripts\windows\installer.iss
; Silent install (what install.ps1 runs):  RemoteDuoHost-Windows-Setup.exe /SILENT

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#define AppExe "remote-duo-host.exe"

[Setup]
; Never change AppId: it's how upgrades find the existing install.
AppId={{894B648B-2C0F-4D54-ABFB-8B1C63010DC2}
AppName=Remote Duo Host
AppVersion={#AppVersion}
AppVerName=Remote Duo Host {#AppVersion}
AppPublisher=iRunTech
AppPublisherURL=https://github.com/iRunTech/remote-duo
AppSupportURL=https://github.com/iRunTech/remote-duo/issues
DefaultDirName={autopf}\Remote Duo
DisableProgramGroupPage=yes
DisableDirPage=yes
OutputDir=..\..\dist
OutputBaseFilename=RemoteDuoHost-Windows-Setup
Compression=lzma2
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
PrivilegesRequired=admin
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName=Remote Duo Host
WizardStyle=modern

[Files]
Source: "..\..\host\target\release\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion
Source: "..\..\THIRD-PARTY-NOTICES.md"; DestDir: "{app}"; DestName: "THIRD-PARTY-NOTICES.txt"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Remote Duo Host"; Filename: "{app}\{#AppExe}"

[Run]
; Let phones reach it on every network type: Windows 11 marks most new Wi-Fi
; as Public, which blocked phones that could still see the host. Pairing needs
; the PIN shown on this PC, and wrong PINs are rate-limited.
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""Remote Duo Host"""; Flags: runhidden
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall add rule name=""Remote Duo Host"" dir=in action=allow program=""{app}\{#AppExe}"" profile=any enable=yes"; Flags: runhidden
; The sign-in screen service (see host/src/secure). "create" fails harmlessly
; on upgrades, so "config" then points it at this install either way.
Filename: "{sys}\sc.exe"; Parameters: "create RemoteDuoSignIn binPath= ""\""{app}\{#AppExe}\"" --service"" start= auto DisplayName= ""Remote Duo Sign-in Helper"""; Flags: runhidden
Filename: "{sys}\sc.exe"; Parameters: "config RemoteDuoSignIn binPath= ""\""{app}\{#AppExe}\"" --service"" start= auto"; Flags: runhidden
Filename: "{sys}\sc.exe"; Parameters: "description RemoteDuoSignIn ""Lets the Remote Duo iPhone app use the Windows sign-in screen."""; Flags: runhidden
Filename: "{sys}\sc.exe"; Parameters: "failure RemoteDuoSignIn reset= 86400 actions= restart/5000/restart/5000/restart/60000"; Flags: runhidden
Filename: "{sys}\sc.exe"; Parameters: "start RemoteDuoSignIn"; Flags: runhidden
Filename: "{app}\{#AppExe}"; Description: "Start Remote Duo Host"; Flags: nowait postinstall runasoriginaluser

[UninstallRun]
Filename: "{sys}\sc.exe"; Parameters: "stop RemoteDuoSignIn"; Flags: runhidden; RunOnceId: "StopService"
Filename: "{sys}\sc.exe"; Parameters: "delete RemoteDuoSignIn"; Flags: runhidden; RunOnceId: "DeleteService"
Filename: "{sys}\taskkill.exe"; Parameters: "/F /IM {#AppExe}"; Flags: runhidden; RunOnceId: "StopHost"
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""Remote Duo Host"""; Flags: runhidden; RunOnceId: "RemoveFirewallRule"

[Registry]
; "Start at Login" (set from the tray menu) lives here; drop it on uninstall.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: none; ValueName: "Remote Duo Host"; Flags: uninsdeletevalue dontcreatekey

[Code]
// The tray app has no window for Windows to ask to close, so stop it (and the
// sign-in service and helper, which are the same exe) before replacing the
// exe on upgrade.
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Code: Integer;
begin
  Exec(ExpandConstant('{sys}\sc.exe'), 'stop RemoteDuoSignIn', '', SW_HIDE, ewWaitUntilTerminated, Code);
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /IM {#AppExe}', '', SW_HIDE, ewWaitUntilTerminated, Code);
  Result := '';
end;

[UninstallDelete]
; Written by the sign-in service next to the exe.
Type: files; Name: "{app}\sign-in.log"
; "Start Automatically"'s copy of the identity and paired phones, in SYSTEM's profile.
Type: filesandordirs; Name: "{sys}\config\systemprofile\AppData\Roaming\remote-duo"
