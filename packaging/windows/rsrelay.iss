; Inno Setup 6 Installer Script for rsrelay Desktop (Windows 10/11 x64)
; Note: Saved as UTF-8 with BOM so `iscc.exe` preserves Chinese text on any host locale.

#ifndef MyAppVersion
  #define MyAppVersion "0.1.0"
#endif

#ifndef BinPath
  #define BinPath "..\..\target\release\rsrelay-gui.exe"
#endif

#ifndef OutDir
  #define OutDir "..\..\dist"
#endif

#define MyAppName "rsrelay"
#define MyAppExeName "rsrelay-gui.exe"
#define MyAppPublisher "rsrelay contributors"
#define MyAppURL "https://github.com/kgbow/rsrelay"
#define FirewallRuleName "rsrelay Audio Relay"

[Setup]
AppId={{8F6D3C9A-54B2-4E8D-9C71-2A4E801F5B32}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppVerName={#MyAppName} v{#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}
AppUpdatesURL={#MyAppURL}/releases
DefaultDirName={autopf}\{#MyAppName}
DefaultGroupName={#MyAppName}
DisableProgramGroupPage=yes
#if VER >= EncodeVer(6,3,0,0)
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
#else
ArchitecturesAllowed=x64
ArchitecturesInstallIn64BitMode=x64
#endif
MinVersion=10.0
PrivilegesRequired=admin
OutputDir={#OutDir}
OutputBaseFilename=rsrelay-v{#MyAppVersion}-windows-x64-setup
SetupIconFile=..\assets\rsrelay.ico
UninstallDisplayIcon={app}\{#MyAppExeName}
UninstallDisplayName={#MyAppName}
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
CloseApplications=force

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"
#if FileExists(SourcePath + "ChineseSimplified.isl")
Name: "zh"; MessagesFile: "ChineseSimplified.isl"
#elif FileExists(AddBackslash(CompilerPath) + "Languages\ChineseSimplified.isl")
Name: "zh"; MessagesFile: "compiler:Languages\ChineseSimplified.isl"
#elif FileExists(AddBackslash(CompilerPath) + "Languages\Unofficial\ChineseSimplified.isl")
Name: "zh"; MessagesFile: "compiler:Languages\Unofficial\ChineseSimplified.isl"
#endif

[CustomMessages]
en.TaskDesktopIcon=Create a &desktop shortcut
en.TaskFirewall=Add Windows Firewall LAN allow rule (recommended for UDP audio & mDNS discovery)
en.TaskAutostart=Start rsrelay automatically in system tray when Windows starts (--tray)
en.GroupNetworkAndStartup=Network & Startup Options:
en.LaunchApp=Launch rsrelay
#if FileExists(SourcePath + "ChineseSimplified.isl") || FileExists(AddBackslash(CompilerPath) + "Languages\ChineseSimplified.isl") || FileExists(AddBackslash(CompilerPath) + "Languages\Unofficial\ChineseSimplified.isl")
zh.TaskDesktopIcon=创建桌面快捷方式(&D)
zh.TaskFirewall=添加 Windows 防火墙局域网放行规则（推荐，放行 UDP 音频流与设备自动发现）
zh.TaskAutostart=开机自动后台启动并驻留系统托盘 (--tray)
zh.GroupNetworkAndStartup=网络与开机启动选项：
zh.LaunchApp=立即运行 rsrelay
#endif

[Tasks]
Name: "desktopicon"; Description: "{cm:TaskDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"
Name: "firewall"; Description: "{cm:TaskFirewall}"; GroupDescription: "{cm:GroupNetworkAndStartup}"
Name: "autostart"; Description: "{cm:TaskAutostart}"; GroupDescription: "{cm:GroupNetworkAndStartup}"; Flags: unchecked

[Files]
Source: "{#BinPath}"; DestDir: "{app}"; DestName: "{#MyAppExeName}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Comment: "Ultra-Low-Latency LAN Audio & Microphone Relay"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon; Comment: "Ultra-Low-Latency LAN Audio & Microphone Relay"

[Registry]
; Clean up autostart entry if user unchecks it on upgrade, or write it when checked
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: none; ValueName: "rsrelay"; Flags: deletevalue
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "rsrelay"; ValueData: """{app}\{#MyAppExeName}"" --tray"; Tasks: autostart; Flags: uninsdeletevalue

[Run]
; Remove stale rules before adding clean inbound/outbound rules for rsrelay-gui.exe
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""{#FirewallRuleName}"""; Tasks: firewall; Flags: runhidden
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall add rule name=""{#FirewallRuleName}"" dir=in action=allow program=""{app}\{#MyAppExeName}"" enable=yes profile=any"; Tasks: firewall; Flags: runhidden
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall add rule name=""{#FirewallRuleName}"" dir=out action=allow program=""{app}\{#MyAppExeName}"" enable=yes profile=any"; Tasks: firewall; Flags: runhidden
Filename: "{app}\{#MyAppExeName}"; Description: "{cm:LaunchApp}"; Flags: nowait postinstall skipifsilent

[UninstallRun]
Filename: "{sys}\taskkill.exe"; Parameters: "/F /IM {#MyAppExeName}"; Flags: runhidden; RunOnceId: "StopRunningTrayApp"
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""{#FirewallRuleName}"""; Flags: runhidden; RunOnceId: "RemoveFirewallRule"

[Code]
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  ResultCode: Integer;
begin
  // Ensure any background tray instance is stopped before overwriting rsrelay-gui.exe
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /IM {#MyAppExeName}', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  Result := '';
end;
