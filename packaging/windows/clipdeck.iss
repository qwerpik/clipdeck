; Inno Setup script for Clipdeck. Build: iscc /DVersion=0.1.0 packaging\windows\clipdeck.iss
#ifndef Version
  #define Version "0.1.0"
#endif

[Setup]
AppId={{6A1C3E52-6E1B-4C2B-9E4A-2C6F3A5B7D10}
AppName=Clipdeck
AppVersion={#Version}
AppPublisher=Clipdeck contributors
AppPublisherURL=https://github.com/qwerpik/clipdeck
DefaultDirName={autopf}\Clipdeck
DefaultGroupName=Clipdeck
DisableProgramGroupPage=yes
LicenseFile=..\..\LICENSE
OutputDir=..\..\dist
OutputBaseFilename=Clipdeck-{#Version}-setup-x64
SetupIconFile=..\..\assets\clipdeck.ico
UninstallDisplayIcon={app}\clipdeck.exe
Compression=lzma2/max
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
WizardStyle=modern

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "micfx"; Description: "Put Clipdeck's sounds into my microphone (no virtual cable; asks for administrator rights)"; GroupDescription: "Microphone:"; Flags: unchecked
Name: "autostart"; Description: "Start Clipdeck when I sign in"; GroupDescription: "Startup:"; Flags: unchecked

[Files]
Source: "..\..\target\release\clipdeck.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\target\release\clipdeck_micfx.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\README.md"; DestDir: "{app}"; Flags: ignoreversion isreadme
Source: "..\..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\Clipdeck"; Filename: "{app}\clipdeck.exe"
Name: "{autodesktop}\Clipdeck"; Filename: "{app}\clipdeck.exe"; Tasks: desktopicon
Name: "{userstartup}\Clipdeck"; Filename: "{app}\clipdeck.exe"; Parameters: "--minimized"; Tasks: autostart

[Run]
Filename: "{app}\clipdeck.exe"; Parameters: "micfx install default"; StatusMsg: "Adding Clipdeck to your microphone..."; Tasks: micfx; Flags: runhidden waituntilterminated
Filename: "{app}\clipdeck.exe"; Description: "{cm:LaunchProgram,Clipdeck}"; Flags: nowait postinstall skipifsilent

[UninstallRun]
; Removes the in-mic effect and restores the microphone settings (no-op when not installed).
Filename: "{app}\clipdeck.exe"; Parameters: "micfx uninstall"; RunOnceId: "micfx"; Flags: runhidden waituntilterminated
