[Setup]
AppId={{A19D4A26-6B0D-4C7A-9B11-000000000001}
AppName=Rexium Launcher
AppVersion=0.2.0
AppPublisher=Silver Dev Studios
DefaultDirName={localappdata}\Programs\Rexium Launcher
DefaultGroupName=Rexium Launcher
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesInstallIn64BitMode=x64
OutputDir=output
OutputBaseFilename=RexiumLauncherSetup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern

[Files]
Source: "..\target\release\rexium-launcher.exe"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\Rexium Launcher"; Filename: "{app}\rexium-launcher.exe"
Name: "{autodesktop}\Rexium Launcher"; Filename: "{app}\rexium-launcher.exe"; Tasks: desktopicon

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Additional shortcuts:"

[Run]
Filename: "{app}\rexium-launcher.exe"; Description: "Launch Rexium Launcher"; Flags: nowait postinstall skipifsilent
