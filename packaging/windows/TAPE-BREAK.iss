#define MyAppName "TAPE//BREAK"
#define MyAppVersion "0.1.0"
#define MyAppPublisher "TAPE//BREAK"
#define MyAppExeName "TAPE-BREAK.exe"

[Setup]
AppId={{C56B7E7D-0A1D-4D75-A1A8-8B2F0B37A6F4}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher={#MyAppPublisher}
DefaultDirName={autopf}\TAPE-BREAK
DefaultGroupName={#MyAppName}
UninstallDisplayIcon={app}\{#MyAppExeName}
OutputDir=..\..\dist\installer
OutputBaseFilename=TAPE-BREAK-Setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ArchitecturesInstallIn64BitMode=x64
PrivilegesRequired=admin
SetupLogging=yes

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Additional shortcuts:"; Flags: unchecked

[Files]
Source: "..\..\dist\TAPE-BREAK\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{group}\TAPE//BREAK"; Filename: "{app}\{#MyAppExeName}"
Name: "{autodesktop}\TAPE//BREAK"; Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "Launch TAPE//BREAK"; Flags: postinstall nowait skipifsilent
