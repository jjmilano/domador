; Domador - instalador per-user para Windows 10/11 x64 (Inno Setup 6.3+).
;
; Este archivo lo consume installer/build.ps1, que define AppVersion,
; NumericVersion, StageDir, OutputDir y los hashes SHA-256 del payload.
; Los #ifndef permiten compilar a mano sin pasar defines, pero un build de
; release SIEMPRE debe pasar los hashes: si quedan vacios, la verificacion
; post-instalacion se saltea.
;
; Decisiones:
;  - PrivilegesRequired=lowest: instalar o desinstalar NO pide UAC.
;  - DefaultDirName fijo en %LOCALAPPDATA%\Programs\Domador porque
;    scripts/install-admin.ps1 (backend) resuelve esa ruta exacta.
;  - No se toca %LOCALAPPDATA%\Domador al desinstalar: ahi vive config.json.

#ifndef AppVersion
  #define AppVersion "0.6.2"
#endif
#ifndef NumericVersion
  #define NumericVersion "0.6.2.0"
#endif
#ifndef StageDir
  #define StageDir "..\dist\stage"
#endif
#ifndef OutputDir
  #define OutputDir "..\dist"
#endif
#ifndef HashDomador
  #define HashDomador ""
#endif
#ifndef HashDomadorUi
  #define HashDomadorUi ""
#endif
#ifndef HashDomadorTray
  #define HashDomadorTray ""
#endif
#ifndef HashAdminScript
  #define HashAdminScript ""
#endif

[Setup]
AppId={{6C1F0E7A-3B4D-4C9E-9E2A-7D5B8A1C4F60}
AppName=Domador
AppVersion={#AppVersion}
AppVerName=Domador {#AppVersion}
AppPublisher=José Milano
AppPublisherURL=https://github.com/jjmilano/domador
AppSupportURL=https://github.com/jjmilano/domador/issues
AppUpdatesURL=https://github.com/jjmilano/domador/releases
VersionInfoVersion={#NumericVersion}
VersionInfoCompany=José Milano
VersionInfoDescription=Instalador de Domador
VersionInfoProductName=Domador
VersionInfoProductVersion={#NumericVersion}
VersionInfoCopyright=MIT
DefaultDirName={localappdata}\Programs\Domador
DisableDirPage=yes
DefaultGroupName=Domador
DisableProgramGroupPage=yes
LicenseFile=..\LICENSE
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
OutputDir={#OutputDir}
OutputBaseFilename=Domador-{#AppVersion}-x64-setup
SetupIconFile=..\assets\domador.ico
UninstallDisplayIcon={app}\domador-ui.exe
UninstallDisplayName=Domador {#AppVersion}
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ShowLanguageDialog=auto
SetupLogging=yes
CloseApplications=yes
RestartApplications=no
ChangesAssociations=no

[Languages]
Name: "spanish"; MessagesFile: "compiler:Languages\Spanish.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "startup_tray"; Description: "Iniciar la bandeja al iniciar sesión (sin privilegios de administrador)"; GroupDescription: "Arranque automático:"; Flags: unchecked
Name: "startup_admin"; Description: "Arrancar elevado en cada inicio con una tarea programada (pide UAC una vez)"; GroupDescription: "Arranque automático:"; Flags: unchecked

[Files]
Source: "{#StageDir}\domador.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StageDir}\domador-ui.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StageDir}\domador-tray.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StageDir}\install-admin.ps1"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StageDir}\activate-admin.cmd"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StageDir}\check-update.ps1"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StageDir}\verify-hashes.ps1"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StageDir}\SHA256SUMS.txt"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\Domador"; Filename: "{app}\domador-ui.exe"; WorkingDir: "{app}"; Comment: "Cabina de control nativa"
Name: "{group}\Domador (panel web)"; Filename: "{app}\domador.exe"; WorkingDir: "{app}"; Comment: "Motor y panel web en 127.0.0.1:8787"
Name: "{group}\Domador (bandeja)"; Filename: "{app}\domador-tray.exe"; WorkingDir: "{app}"; Comment: "Icono de bandeja, sin ventana"
Name: "{group}\Activar arranque como administrador"; Filename: "{app}\activate-admin.cmd"; WorkingDir: "{app}"; IconFilename: "{app}\domador-ui.exe"; Comment: "Crea la tarea programada Domador (UAC)"
Name: "{group}\Buscar actualizaciones"; Filename: "powershell.exe"; Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\check-update.ps1"""; WorkingDir: "{app}"; IconFilename: "{app}\domador-ui.exe"; Comment: "Verificación manual y opt-in"
Name: "{group}\{cm:UninstallProgram,Domador}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\Domador"; Filename: "{app}\domador-ui.exe"; WorkingDir: "{app}"; Tasks: desktopicon
Name: "{userstartup}\Domador (bandeja)"; Filename: "{app}\domador-tray.exe"; WorkingDir: "{app}"; Tasks: startup_tray; Check: AllowStartupIcon

[Run]
Filename: "{app}\domador-ui.exe"; Description: "{cm:LaunchProgram,Domador}"; Flags: nowait postinstall skipifsilent

[UninstallRun]
Filename: "taskkill.exe"; Parameters: "/F /IM domador-tray.exe /IM domador-ui.exe /IM domador.exe"; Flags: runhidden; RunOnceId: "StopDomador"

[Code]
function PSQuote(const Value: String): String;
var
  S: String;
begin
  S := Value;
  StringChangeEx(S, '''', '''''', True);
  Result := '''' + S + '''';
end;

function Sha256OfFile(const FileName: String): String;
var
  TmpFile, Params: String;
  Code: Integer;
  Data: AnsiString;
begin
  Result := '';
  TmpFile := ExpandConstant('{tmp}\domador-sha256.txt');
  DeleteFile(TmpFile);
  Params := '-NoProfile -NonInteractive -ExecutionPolicy Bypass -Command "' +
    '$p=' + PSQuote(FileName) + ';' +
    'if(Test-Path -LiteralPath $p){' +
    '$f=[IO.File]::OpenRead($p);' +
    'try{$h=[Security.Cryptography.SHA256]::Create().ComputeHash($f)}finally{$f.Close()};' +
    '[BitConverter]::ToString($h).Replace([string][char]45,[string]::Empty).ToLower()|' +
    'Set-Content -LiteralPath ' + PSQuote(TmpFile) + ' -NoNewline}"';
  if Exec('powershell.exe', Params, '', SW_HIDE, ewWaitUntilTerminated, Code) and (Code = 0) and FileExists(TmpFile) then
    if LoadStringFromFile(TmpFile, Data) then
      Result := Trim(String(Data));
  DeleteFile(TmpFile);
end;

function HashOk(const FilePath, Expected: String): Boolean;
begin
  if Expected = '' then
    Result := True
  else
    Result := Sha256OfFile(FilePath) = Lowercase(Expected);
end;

procedure VerifyInstalledFiles();
var
  Bad: String;
begin
  Bad := '';
  if not HashOk(ExpandConstant('{app}\domador.exe'), '{#HashDomador}') then
    Bad := Bad + 'domador.exe' + #13#10;
  if not HashOk(ExpandConstant('{app}\domador-ui.exe'), '{#HashDomadorUi}') then
    Bad := Bad + 'domador-ui.exe' + #13#10;
  if not HashOk(ExpandConstant('{app}\domador-tray.exe'), '{#HashDomadorTray}') then
    Bad := Bad + 'domador-tray.exe' + #13#10;
  if not HashOk(ExpandConstant('{app}\install-admin.ps1'), '{#HashAdminScript}') then
    Bad := Bad + 'install-admin.ps1' + #13#10;

  if Bad <> '' then
    MsgBox('La verificación SHA-256 de los archivos instalados falló para:' + #13#10 + #13#10 +
      Bad + #13#10 +
      'El instalador puede estar corrupto o manipulado. No uses esta instalación: ' +
      'volvé a descargar el release y comprobá su hash con verify-hashes.ps1.',
      mbError, MB_OK);
end;

function ScheduledTaskExists(): Boolean;
var
  Code: Integer;
begin
  Result := Exec('schtasks.exe', '/Query /TN "Domador"', '', SW_HIDE,
    ewWaitUntilTerminated, Code) and (Code = 0);
end;

function AllowStartupIcon(): Boolean;
begin
  // La tarea elevada ya arranca la bandeja; el Startup duplicado sobra.
  Result := not WizardIsTaskSelected('startup_admin');
end;

function RunAdminTask(): Boolean;
var
  Code: Integer;
begin
  Code := -1;
  Result := ShellExec('runas', 'powershell.exe',
    '-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File "' +
      ExpandConstant('{app}\install-admin.ps1') + '" -Start',
    ExpandConstant('{app}'), SW_HIDE, ewWaitUntilTerminated, Code);
  if not Result then
    MsgBox('No se creó la tarea elevada (UAC cancelado o bloqueado por política).' + #13#10 +
      'Podés activarla más tarde desde Inicio > Domador > ' +
      '"Activar arranque como administrador".', mbInformation, MB_OK)
  else if Code <> 0 then
    MsgBox('La tarea elevada no se pudo crear (código ' + IntToStr(Code) + ').' + #13#10 +
      'Revisá que la política de tareas programadas lo permita o usá el acceso directo ' +
      '"Activar arranque como administrador".', mbError, MB_OK);
end;

function InitializeSetup(): Boolean;
begin
  Result := True;
  if not IsWin64 then
  begin
    MsgBox('Domador requiere Windows de 64 bits (x64).', mbCriticalError, MB_OK);
    Result := False;
  end;
end;

function NextButtonClick(CurPageID: Integer): Boolean;
begin
  Result := True;
  if CurPageID = wpSelectTasks then
    if WizardIsTaskSelected('startup_admin') and WizardIsTaskSelected('startup_tray') then
      MsgBox('Elegiste la tarea elevada y el Startup común.' + #13#10 + #13#10 +
        'La tarea elevada reemplaza al Startup: no se va a crear el acceso en Startup ' +
        'para no arrancar Domador dos veces.', mbInformation, MB_OK);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Code: Integer;
begin
  Result := '';
  NeedsRestart := False;
  // Cerrar instancias propias antes de reemplazar binarios. Si la tarea
  // elevada está corriendo, taskkill sin admin falla y el archivo queda
  // bloqueado: Inno muestra el error de copia y el usuario cierra la bandeja.
  Exec('taskkill.exe', '/F /IM domador-tray.exe /IM domador-ui.exe /IM domador.exe',
    '', SW_HIDE, ewWaitUntilTerminated, Code);
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
  begin
    VerifyInstalledFiles();
    if WizardIsTaskSelected('startup_admin') then
      RunAdminTask();
  end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Code: Integer;
begin
  if CurUninstallStep = usUninstall then
  begin
    if (not UninstallSilent) and ScheduledTaskExists() then
      if MsgBox('Domador tiene una tarea programada elevada ("Domador").' + #13#10 + #13#10 +
        '¿Querés eliminarla ahora? Windows va a pedir permiso de administrador.' + #13#10 +
        'Si decís No, la tarea queda apuntando a un ejecutable que ya no existe.',
        mbConfirmation, MB_YESNO) = IDYES then
        ShellExec('runas', 'schtasks.exe', '/Delete /TN "Domador" /F', '',
          SW_HIDE, ewWaitUntilTerminated, Code);
  end
  else if CurUninstallStep = usPostUninstall then
  begin
    if not UninstallSilent then
      MsgBox('Domador se desinstaló.' + #13#10 + #13#10 +
        'Tu configuración, perfiles y reglas quedaron intactos en:' + #13#10 +
        ExpandConstant('{localappdata}\Domador') + #13#10 + #13#10 +
        'Borrá esa carpeta a mano sólo si querés empezar de cero.',
        mbInformation, MB_OK);
  end;
end;
