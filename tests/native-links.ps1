#requires -Version 5.1
<#
Checks Cargo's resolved native dependency graph for the URL-opening backend.
Generate the input with:
  build.cmd metadata --format-version 1 --locked --offline --filter-platform x86_64-pc-windows-msvc
Then run:
  .\tests\native-links.ps1 -Metadata .build/<run>/metadata.json
This test does not run Cargo or open a browser.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Metadata
)

$ErrorActionPreference = 'Stop'
$graph = Get-Content -LiteralPath $Metadata -Raw | ConvertFrom-Json
$packagesById = @{}
foreach ($package in $graph.packages) { $packagesById[$package.id] = $package }
$nodesById = @{}
foreach ($node in $graph.resolve.nodes) { $nodesById[$node.id] = $node }

$root = $nodesById[$graph.resolve.root]
if (-not $root -or $packagesById[$root.id].name -ne 'bnauth-desktop') {
    throw 'Metadata must resolve the bnauth-desktop package as its root.'
}

function Get-NormalDependency($FromNode, [string]$PackageName) {
    $edges = @($FromNode.deps | Where-Object {
        $packagesById[$_.pkg].name -eq $PackageName -and
        @($_.dep_kinds | Where-Object { $null -eq $_.kind }).Count -gt 0
    })
    if ($edges.Count -ne 1 -or -not $nodesById.ContainsKey($edges[0].pkg)) {
        throw ('Missing resolved normal dependency: {0} -> {1}.' -f $packagesById[$FromNode.id].name, $PackageName)
    }
    return $nodesById[$edges[0].pkg]
}

$eframe = Get-NormalDependency $root 'eframe'
if ('links' -notin $eframe.features) { throw 'The resolved eframe dependency has no links feature.' }
$eguiWinit = Get-NormalDependency $eframe 'egui-winit'
if ('links' -notin $eguiWinit.features -or 'webbrowser' -notin $eguiWinit.features) {
    throw 'The resolved native egui-winit dependency has no browser-opening backend.'
}
$webbrowser = Get-NormalDependency $eguiWinit 'webbrowser'
Write-Output ('PASS: bnauth-desktop -> eframe/links -> egui-winit/links -> webbrowser {0} is resolved for the native build.' -f $packagesById[$webbrowser.id].version)
