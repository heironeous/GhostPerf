<#
.SYNOPSIS
    Works out the semantic version a CI release of a commit should get, and
    optionally stamps it into Cargo.toml's [workspace.package] version.

.DESCRIPTION
    Git tags (vMAJOR.MINOR.PATCH) are the source of truth for released
    versions; Cargo.toml isn't committed back to on every release (that would
    need a bot push to main, which re-triggers CI and fights branch
    protection). Instead, CI stamps the computed version into its own checkout
    before building, so every exe's embedded version info, the installer and
    the zip all agree with the tag.

    Rules, in order:
      1. The commit already carries a vX.Y.Z tag (a re-run of a release
         workflow): reuse that version, so a re-run never mints a second tag.
      2. No vX.Y.Z tag exists anywhere yet: the first release uses Cargo.toml's
         version as is.
      3. Cargo.toml's version is higher than the latest tag: use it as is.
         This is the manual override, e.g. set it to 1.0.0 to cut 1.0.0.
      4. Otherwise bump the latest tag. -Bump picks the part explicitly; with
         -Bump auto (the default) the commit messages since the latest tag
         decide:
           major: "#major", "BREAKING CHANGE:", or a Conventional Commits "!"
                  ("feat!: ...", "fix(ui)!: ...")
           minor: "#minor", or a Conventional Commits "feat:"/"feat(scope):"
           patch: anything else
         so a push with plain messages is a bugfix release.

    Writes `version`, `tag` and `existing` (true/false) to $GITHUB_OUTPUT
    when running in GitHub Actions; always prints them.

.PARAMETER Bump
    auto (default), patch, minor or major.

.PARAMETER Ref
    The commit being released (default HEAD).

.PARAMETER Apply
    Also write the computed version into Cargo.toml's [workspace.package].
#>
param(
    [ValidateSet("auto", "patch", "minor", "major")]
    [string]$Bump = "auto",
    [string]$Ref = "HEAD",
    [switch]$Apply
)

$ErrorActionPreference = "Stop"
$repoRoot = Resolve-Path (Join-Path $PSScriptRoot "..\..")
$cargoToml = Join-Path $repoRoot "Cargo.toml"

# Only the version line inside [workspace.package]: dependency tables further
# down also have `version = "..."` keys. [^\[]*? stops at the next table header.
$workspaceVersionPattern = '(?ms)(^\[workspace\.package\][^\[]*?^\s*version\s*=\s*")([^"]+)(")'

function ConvertTo-SemVer([string]$text) {
    if ($text -match '^v?(\d+)\.(\d+)\.(\d+)$') {
        return [version]::new([int]$Matches[1], [int]$Matches[2], [int]$Matches[3])
    }
    return $null
}

function Format-SemVer([version]$v) { "$($v.Major).$($v.Minor).$($v.Build)" }

function Get-VersionTags([string[]]$names) {
    @($names | ForEach-Object { ConvertTo-SemVer $_ } | Where-Object { $_ } | Sort-Object -Descending)
}

function Invoke-Git {
    $out = & git -C $repoRoot @args
    if ($LASTEXITCODE -ne 0) { throw "git $args failed with exit code $LASTEXITCODE" }
    $out
}

$cargoText = Get-Content $cargoToml -Raw
$cargoMatch = [regex]::Match($cargoText, $workspaceVersionPattern)
if (-not $cargoMatch.Success) { throw "No version under [workspace.package] in $cargoToml" }
$cargoVersion = ConvertTo-SemVer $cargoMatch.Groups[2].Value
if (-not $cargoVersion) { throw "Cargo.toml version '$($cargoMatch.Groups[2].Value)' isn't MAJOR.MINOR.PATCH" }

$existing = $false
$onCommit = @(Get-VersionTags (Invoke-Git tag --points-at $Ref --list "v*"))
$allTags = @(Get-VersionTags (Invoke-Git tag --list "v*"))

if ($onCommit.Count -gt 0) {
    $next = $onCommit[0]
    $existing = $true
    $reason = "commit is already tagged v$(Format-SemVer $next)"
}
elseif ($allTags.Count -eq 0) {
    $next = $cargoVersion
    $reason = "no release tags yet; using Cargo.toml's version"
}
elseif ($cargoVersion -gt $allTags[0]) {
    $next = $cargoVersion
    $reason = "Cargo.toml's version is ahead of the latest tag v$(Format-SemVer $allTags[0])"
}
else {
    $latest = $allTags[0]
    $part = $Bump
    if ($part -eq "auto") {
        $latestTag = "v$(Format-SemVer $latest)"
        # The latest tag might not be an ancestor of $Ref (e.g. a release cut
        # from another branch); then fall back to the last 100 commits rather
        # than the whole history.
        & git -C $repoRoot merge-base --is-ancestor $latestTag $Ref 2>$null
        # Outer @(): an `if` returning a one-element array unwraps it to a
        # string, which splatting then passes one character at a time (same
        # gotcha as build.ps1's $profileArgs).
        $range = @(if ($LASTEXITCODE -eq 0) { "$latestTag..$Ref" } else { "-n", "100", $Ref })
        $messages = (Invoke-Git log --format=%B @range) -join "`n"

        $part = "patch"
        if ($messages -match '(?im)#major\b|^BREAKING[ -]CHANGE:|^\w+(\([^)]*\))?!:') {
            $part = "major"
        }
        elseif ($messages -match '(?im)#minor\b|^feat(\([^)]*\))?:') {
            $part = "minor"
        }
    }
    $next = switch ($part) {
        "major" { [version]::new($latest.Major + 1, 0, 0) }
        "minor" { [version]::new($latest.Major, $latest.Minor + 1, 0) }
        "patch" { [version]::new($latest.Major, $latest.Minor, $latest.Build + 1) }
    }
    $reason = "$part bump from v$(Format-SemVer $latest)"
}

$version = Format-SemVer $next
$tag = "v$version"
Write-Host "Version $version ($reason)"

if ($Apply) {
    $stamped = ([regex]$workspaceVersionPattern).Replace($cargoText, '${1}' + $version + '${3}', 1)
    Set-Content -Path $cargoToml -Value $stamped -NoNewline
    Write-Host "Stamped $version into Cargo.toml [workspace.package]"
}

if ($env:GITHUB_OUTPUT) {
    "version=$version" | Out-File -FilePath $env:GITHUB_OUTPUT -Append -Encoding utf8
    "tag=$tag" | Out-File -FilePath $env:GITHUB_OUTPUT -Append -Encoding utf8
    "existing=$($existing.ToString().ToLower())" | Out-File -FilePath $env:GITHUB_OUTPUT -Append -Encoding utf8
}
