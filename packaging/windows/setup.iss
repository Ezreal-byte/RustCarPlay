; Build with package-installers.py. No driver, service or runtime installation.
[Setup]
AppId={code:InstallationId}
AppName=RustCarPlay
AppVersion={#ProductVersion}
AppPublisher=RustCarPlay contributors
AppPublisherURL=https://github.com/Ezreal-byte/RustCarPlay
DefaultDirName={localappdata}\Programs\RustCarPlay
DefaultGroupName=RustCarPlay
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.20348
OutputDir={#OutputDirectory}
OutputBaseFilename={#OutputName}
SetupIconFile={#PackageRoot}\resources\RustCarPlay.ico
UninstallDisplayIcon={app}\resources\RustCarPlay.ico
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
DisableWelcomePage=no
ShowLanguageDialog=yes
LanguageDetectionMethod=uilanguage
UsePreviousLanguage=no
CloseApplications=no
RestartApplications=no
Uninstallable=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "chinesesimplified"; MessagesFile: "compiler:Default.isl,{#ChineseMessages}"

[CustomMessages]
english.DesktopShortcut=Create a desktop shortcut
chinesesimplified.DesktopShortcut=创建桌面快捷方式
english.StartApplication=Launch RustCarPlay
chinesesimplified.StartApplication=启动 RustCarPlay
english.UninstallShortcut=Uninstall RustCarPlay
chinesesimplified.UninstallShortcut=卸载 RustCarPlay

[Tasks]
Name: "desktopicon"; Description: "{cm:DesktopShortcut}"; Flags: unchecked

[Files]
Source: "{#PackageRoot}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{group}\RustCarPlay"; Filename: "{app}\RustCarPlay.exe"; IconFilename: "{app}\resources\RustCarPlay.ico"
Name: "{group}\{cm:UninstallShortcut}"; Filename: "{uninstallexe}"
Name: "{userdesktop}\RustCarPlay"; Filename: "{app}\RustCarPlay.exe"; IconFilename: "{app}\resources\RustCarPlay.ico"; Tasks: desktopicon

[Run]
Filename: "{app}\RustCarPlay.exe"; Description: "{cm:StartApplication}"; Flags: postinstall nowait skipifsilent unchecked

[Code]
function InstallationId(Param: String): String;
var
  SmokeId: String;
begin
  SmokeId := ExpandConstant('{param:SmokeTestId|}');
  if SmokeId = '' then
    Result := '{A4B82BCC-7B69-4CC9-B413-1A42F2AB577E}'
  else
    Result := 'RustCarPlay-Packaging-Smoke-' + SmokeId;
end;

// Deliberately no [UninstallDelete]: settings and pairing records live in the
// user's data directory and survive both an upgrade and application removal.
