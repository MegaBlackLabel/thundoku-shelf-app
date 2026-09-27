# thundoku-shelf: Google Drive の同期フォルダ健全性チェック（読み取り専用）
#
# 背景: アプリは `drive.sync.folder_id` が無いとき `create_folder("thundoku-shelf")` するだけで
# 同名フォルダを探しに行かないため、My Drive 直下に同名フォルダが増えていく（実測で 14 個）。
# どれが現役か・鍵 bundle がどこにあるか・今のローカルの pack と重なっているかを 1 回で出す。
#
# 使い方（Windows / アプリのログイン済みトークンを使う）:
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts/drive-audit.ps1
#   powershell ... -File scripts/drive-audit.ps1 -ActiveFolderId <アプリが使っている ID>
#   powershell ... -File scripts/drive-audit.ps1 -PacksDir "C:\path\to\packs"
#
# 出力:
#   FOLDER   ... My Drive 直下の `thundoku-shelf` フォルダ（id / 作成日時 / 中身の要約）
#   KEYFILE  ... どのフォルダに `thundoku-keys.json`（pack のルート鍵のラップ）があるか
#   OVERLAP  ... ローカルの `packs/*.opfspack` と重なる pack の数（=-PacksDir 指定時）
#   ACTIVE   ... -ActiveFolderId と一致したフォルダ
#
# 読み取り（files.list / files.get）しかしない。削除・移動は一切しない。
# `drive.file` スコープなので「アプリが作った / 利用者が選んだ」ファイルだけが見える
# （手動で作ったフォルダや別クライアントが作ったものは出てこない）。
param(
  [string]$ActiveFolderId = "",
  [string]$PacksDir = "$env:APPDATA\thundoku-shelf\packs",
  [string]$FolderName = "thundoku-shelf",
  [string]$CredTarget = "google.com.megablacklabel.thundoku-shelf",
  [string]$ClientId = "1054619943130-2kaqpgnm719bp8l8rslm8rkuvdhb945s.apps.googleusercontent.com",
  [string]$ClientSecret = "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"
)
$ErrorActionPreference = 'Stop'

# Windows 資格情報マネージャーから OAuth トークン（JSON）を読む。値は出力しない。
$sig = @'
using System;
using System.Runtime.InteropServices;
public class ThundokuCred {
  [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)]
  public struct CREDENTIAL {
    public uint Flags; public uint Type; public string TargetName; public string Comment;
    public System.Runtime.InteropServices.ComTypes.FILETIME LastWritten;
    public uint CredentialBlobSize; public IntPtr CredentialBlob; public uint Persist;
    public uint AttributeCount; public IntPtr Attributes; public string TargetAlias; public string UserName;
  }
  [DllImport("advapi32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
  public static extern bool CredReadW(string target, uint type, uint flags, out IntPtr credential);
  [DllImport("advapi32.dll")] public static extern void CredFree(IntPtr cred);
  public static string Read(string target) {
    IntPtr p;
    if (!CredReadW(target, 1, 0, out p)) { return null; }
    try {
      CREDENTIAL c = (CREDENTIAL)Marshal.PtrToStructure(p, typeof(CREDENTIAL));
      return Marshal.PtrToStringUni(c.CredentialBlob, (int)(c.CredentialBlobSize / 2));
    } finally { CredFree(p); }
  }
}
'@
Add-Type -TypeDefinition $sig -Language CSharp

$raw = [ThundokuCred]::Read($CredTarget)
if (-not $raw) { Write-Output "ERR: keyring entry '$CredTarget' not found (Google にログインしていない？)"; exit 1 }
$json = $raw
if ($raw -notmatch '^\s*\{') {
  try { $json = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($raw)) } catch {}
}
$tok = $json | ConvertFrom-Json
if (-not $tok.refresh_token) { Write-Output 'ERR: keyring entry has no refresh_token'; exit 1 }

$resp = Invoke-RestMethod -Method Post -Uri 'https://oauth2.googleapis.com/token' -ContentType 'application/x-www-form-urlencoded' `
  -Body @{ grant_type = 'refresh_token'; refresh_token = $tok.refresh_token; client_id = $ClientId; client_secret = $ClientSecret }
$H = @{ Authorization = "Bearer $($resp.access_token)" }

function List-Files([string]$q, [string]$fields) {
  $uri = "https://www.googleapis.com/drive/v3/files?spaces=drive&pageSize=1000&fields=files($fields)&q=" + [Uri]::EscapeDataString($q)
  @((Invoke-RestMethod -Uri $uri -Headers $H).files)
}

$local = @()
if (Test-Path $PacksDir) {
  $local = @(Get-ChildItem -Path $PacksDir -Filter '*.opfspack' -File | ForEach-Object { $_.BaseName })
}

Write-Output "=== folders named '$FolderName' in My Drive root ==="
Write-Output ("local packs in {0}: {1}" -f $PacksDir, $local.Count)
# 要素が 1 つのとき関数戻り値が配列から外れるため `@()` で包む（.Count を落とさない）。
$folders = @(List-Files ("mimeType='application/vnd.google-apps.folder' and name='{0}' and 'root' in parents and trashed=false" -f $FolderName) 'id,name,createdTime,modifiedTime')
foreach ($f in $folders) {
  $kids = List-Files ("'{0}' in parents and trashed=false" -f $f.id) 'id,name,size,modifiedTime'
  $bytes = ($kids | Measure-Object -Property size -Sum).Sum
  if (-not $bytes) { $bytes = 0 }
  $keys = @($kids | Where-Object { $_.name -eq 'thundoku-keys.json' })
  $bk = @($kids | Where-Object { $_.name -eq 'thundoku-backup.json' })
  $packs = @($kids | Where-Object { $_.name -like '*.opfspack' })
  $overlap = 0
  foreach ($p in $packs) { if ($local -contains ($p.name -replace '\.opfspack$', '')) { $overlap++ } }
  $tag = if ($ActiveFolderId -and $f.id -eq $ActiveFolderId) { 'ACTIVE' } elseif ($ActiveFolderId) { '-' } else { '' }
  Write-Output ("FOLDER`t{0}`t{1}`tcreated={2}`tmodified={3}`tfiles={4}`tbytes={5}`tpacks={6}`toverlap-local={7}`tkeys={8}`tbackup={9}" -f `
    $f.id, $tag, $f.createdTime, $f.modifiedTime, $kids.Count, $bytes, $packs.Count, $overlap, `
    ($(if ($keys.Count) { "yes(" + $keys[0].modifiedTime + ")" } else { 'no' })), `
    ($(if ($bk.Count) { "yes(" + $bk[0].modifiedTime + ")" } else { 'no' })))
}
Write-Output ("folder count: {0}" -f $folders.Count)
