[Setup]
AppId={{A19D4A26-6B0D-4C7A-9B11-000000000001}
AppName=Tensura Launcher
AppVersion=0.4.0
AppPublisher=Silver Dev Studios
DefaultDirName={localappdata}\Programs\Tensura Launcher
DefaultGroupName=Tensura Launcher
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesInstallIn64BitMode=x64
OutputDir=output
OutputBaseFilename=TensuraLauncherSetup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern

[Files]
Source: "..\target\release\tensura-launcher.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\runtime\*"; DestDir: "{app}\runtime"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{group}\Tensura Launcher"; Filename: "{app}\tensura-launcher.exe"
Name: "{autodesktop}\Tensura Launcher"; Filename: "{app}\tensura-launcher.exe"; Tasks: desktopicon

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Additional shortcuts:"

[Run]
Filename: "{app}\tensura-launcher.exe"; Description: "Launch Tensura Launcher"; Flags: nowait postinstall skipifsilent