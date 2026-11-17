#requires -Version 5.1
<#
Run with Windows PowerShell and an original WinAuth 2 assembly (or a build of
the pinned official source), never with a private authenticator XML:
  build.cmd run --locked --offline -p bnauth-adapters --example winauth2_fixture -- .build/xml-export/fixtures
  powershell -NoProfile -ExecutionPolicy Bypass -File tests/winauth2-compatibility.ps1 -WinAuthExe <WinAuth.exe> -FixtureDirectory .build/xml-export/fixtures
This test blocks HTTP, uses only RFC 6238's public key, and checks the original
reader, the first unmodified CurrentCode call, OTP calculation, config writer
and reader after re-saving. A zero-offset control proves the old reader's
automatic-sync path is blocked. It does not invoke LoadConfig or modify WinAuth
registry settings.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)] [string]$WinAuthExe,
    [Parameter(Mandatory = $true)] [string]$FixtureDirectory
)
$ErrorActionPreference = 'Stop'
$exe = (Resolve-Path -LiteralPath $WinAuthExe).Path
$fixture = (Resolve-Path -LiteralPath $FixtureDirectory).Path
$assembly = [Reflection.Assembly]::LoadFrom($exe)
$core = Join-Path ([IO.Path]::GetDirectoryName($exe)) 'Authenticator.dll'
if (Test-Path -LiteralPath $core) { [void][Reflection.Assembly]::LoadFrom($core) }
Add-Type -TypeDefinition @'
using System;
using System.Net;
public sealed class BNAuthXmlNoNetwork : IWebRequestCreate {
    public static int Attempts;
    public WebRequest Create(Uri uri) {
        Attempts++;
        throw new InvalidOperationException("Network disabled in XML compatibility test");
    }
}
'@
if (-not [Net.WebRequest]::RegisterPrefix('http://', [BNAuthXmlNoNetwork]::new()) -or
    -not [Net.WebRequest]::RegisterPrefix('https://', [BNAuthXmlNoNetwork]::new())) {
    throw 'Unable to install the HTTP/HTTPS network blockers.'
}

function Read-OriginalAuthenticator([xml]$Document) {
    $nodes = $Document.SelectNodes('/WinAuth/authenticator')
    if ($nodes.Count -ne 1) { throw 'Expected exactly one authenticator.' }
    $stream = [IO.MemoryStream]::new([Text.Encoding]::UTF8.GetBytes($nodes[0].OuterXml))
    $before = [BNAuthXmlNoNetwork]::Attempts
    try { return [WindowsAuthenticator.Authenticator]::ReadFromStream($stream, $null, [decimal]2.0) }
    finally {
        $stream.Dispose()
        if ([BNAuthXmlNoNetwork]::Attempts -ne $before) { throw 'Original reader attempted a network request.' }
    }
}
function Read-OriginalCode($Authenticator) {
    $before = [BNAuthXmlNoNetwork]::Attempts
    try {
        # Reflection propagates a property getter exception. PowerShell's normal
        # property syntax can turn such an exception into a null result instead.
        return $Authenticator.GetType().GetProperty('CurrentCode').GetValue($Authenticator, $null)
    } finally {
        if ([BNAuthXmlNoNetwork]::Attempts -ne $before) {
            throw 'Original CurrentCode attempted a network request.'
        }
    }
}
function Assert-InitialCode($Authenticator) {
    $offset = $Authenticator.ServerTimeDiff
    $code = Read-OriginalCode $Authenticator
    if ($code -cnotmatch '^\d{8}$') { throw 'Original initial CurrentCode did not return eight digits.' }
    if ($Authenticator.ServerTimeDiff -ne $offset) { throw 'Original initial CurrentCode changed the exported clock offset.' }
}
function Assert-ZeroOffsetControl([xml]$Document) {
    $control = $Document.CloneNode($true)
    $control.SelectSingleNode('/WinAuth/authenticator/servertimediff').InnerText = '0'
    $auth = Read-OriginalAuthenticator $control
    $before = [BNAuthXmlNoNetwork]::Attempts
    $blocked = $false
    try {
        $null = $auth.GetType().GetProperty('CurrentCode').GetValue($auth, $null)
    } catch {
        $blocked = $true
    }
    if (-not $blocked -or [BNAuthXmlNoNetwork]::Attempts -ne ($before + 1)) {
        throw 'Zero-offset control did not reproduce exactly one blocked automatic-sync request.'
    }
}
function Assert-OriginalCode($Authenticator) {
    # Move the original clock into the middle of RFC 6238's counter=1 interval.
    # Restore its real exported offset afterwards, so save/reload tests it too.
    $offset = $Authenticator.ServerTimeDiff
    try {
        $Authenticator.ServerTimeDiff = 45000 - [WindowsAuthenticator.Authenticator]::CurrentTime
        if ((Read-OriginalCode $Authenticator) -cne '94287082') { throw 'Original OTP does not match RFC 6238.' }
    } finally { $Authenticator.ServerTimeDiff = $offset }
}

foreach ($name in @('with-serial', 'seed-only', 'with-serial-local', 'with-serial-zero', 'seed-only-zero', 'seed-only-offset')) {
    $source = Join-Path $fixture ($name + '.xml')
    $document = [Xml.XmlDocument]::new()
    $document.PreserveWhitespace = $true
    $document.XmlResolver = $null
    $document.Load($source)
    if ($document.DocumentElement.Name -cne 'WinAuth' -or $document.DocumentElement.GetAttribute('version') -cne '2.0') { throw 'Wrong WinAuth XML root.' }
    $ns = [Xml.XmlNamespaceManager]::new($document.NameTable)
    $ns.AddNamespace('b', 'urn:bnauth:metadata:1')
    $expected = @{
        title = ' 繁體 & <title> "'' 😀 '
        note = "備註`r`n第二行`t<&>`"' -- ]]> "
        serial = $(if ($name.StartsWith('with-serial')) { 'US-1234-5678-9012' } else { '' })
        restoreCode = $(if ($name.StartsWith('with-serial')) { 'AbCd123456' } else { '' })
    }
    foreach ($field in $expected.Keys) {
        $node = $document.SelectSingleNode('/WinAuth/b:metadata/b:' + $field, $ns)
        if ($null -eq $node -or $node.InnerText -cne $expected[$field]) { throw ('Metadata mismatch: ' + $field) }
    }
    $raw = $document.SelectSingleNode('/WinAuth/authenticator/secretdata').InnerText
    $auth = Read-OriginalAuthenticator $document
    if ($auth.GetType().FullName -cne 'WindowsAuthenticator.BattleNetAuthenticator' -or $auth.SecretData -cne $raw -or $auth.CodeDigits -ne 8) { throw 'Original reader changed authenticator data.' }
    if ([string]$auth.Serial -cne $expected.serial) { throw 'Original serial mismatch.' }
    $expectedOffset = $(if ($name -eq 'with-serial' -or $name -eq 'seed-only-offset') { -1234 } else { 1 })
    if ($auth.ServerTimeDiff -ne $expectedOffset) { throw 'Unexpected exported clock offset.' }
    Assert-InitialCode $auth
    Assert-OriginalCode $auth
    $config = [WindowsAuthenticator.WinAuthConfig]::new()
    $config.Authenticator = $auth
    $stream = [IO.MemoryStream]::new()
    $settings = [Xml.XmlWriterSettings]::new()
    $settings.Encoding = [Text.UTF8Encoding]::new($false)
    $writer = [Xml.XmlWriter]::Create($stream, $settings)
    $config.WriteXmlString($writer, $false)
    $writer.Flush()
    $bytes = $stream.ToArray()
    $writer.Dispose()
    $stream.Dispose()
    $roundtrip = [Xml.XmlDocument]::new()
    $roundtrip.XmlResolver = $null
    $roundtrip.LoadXml([Text.Encoding]::UTF8.GetString($bytes))
    $reopened = Read-OriginalAuthenticator $roundtrip
    if ($reopened.SecretData -cne $raw -or $reopened.ServerTimeDiff -ne $auth.ServerTimeDiff) { throw 'Original save/reload changed data.' }
    Assert-InitialCode $reopened
    Assert-OriginalCode $reopened
    if ($roundtrip.OuterXml.Contains('urn:bnauth:metadata:1')) { throw 'Expected original writer to drop the extension.' }
    [IO.File]::WriteAllBytes((Join-Path $fixture ($name + '.native-resaved.xml')), $bytes)
    Assert-ZeroOffsetControl $document
    Write-Output ('PASS {0}: metadata exact; original initial CurrentCode and reopened code without network; RFC OTP; save/reload; offset unchanged; zero-offset sync blocked.' -f $name)
}
if ([BNAuthXmlNoNetwork]::Attempts -ne 6) { throw 'Unexpected network attempt outside the six zero-offset controls.' }
Write-Output ('Original assembly version: ' + $assembly.GetName().Version.ToString())
Write-Output 'Network verification: zero requests for normal initial/read/reopen/RFC paths; six blocked zero-offset controls.'
