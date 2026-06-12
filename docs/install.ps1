# semanticastindexer one-line installer (Windows).
#
#   powershell -c "irm https://maadgrom.github.io/semanticastindexer/install.ps1 | iex"
#
# Downloads a prebuilt binary from the latest GitHub Release (no Rust toolchain
# required). Then, unless you pass -Platform/-All/-NonInteractive, it ASKS which
# coding agent(s) to connect. For JSON-based clients, pass -Write to merge the
# config for you; otherwise it prints the snippet and the exact file path.
#
# To pass flags through a one-liner, invoke the downloaded script as a scriptblock
# (works from both cmd and PowerShell):
#
#   powershell -c "& ([scriptblock]::Create((irm https://maadgrom.github.io/semanticastindexer/install.ps1))) -Platform claude-code"
#   powershell -c "& ([scriptblock]::Create((irm https://maadgrom.github.io/semanticastindexer/install.ps1))) -All -Write"
#
param(
    [string]$Platform = "",
    [switch]$All,
    [switch]$NonInteractive,
    [string]$Collection = "source_code",
    [string]$Embedder = "ort",
    [switch]$Write,
    [switch]$SkipBinary,
    [switch]$Help
)

$ErrorActionPreference = "Stop"
# Stock Windows PowerShell 5.1 defaults to TLS 1.0, which GitHub rejects.
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

# --- Repo constants ---
$Owner = "maadgrom"
$Repo = "semanticastindexer"
$BinaryName = "semanticastindexer"
$ServerName = "semantic-code-search"
$ReleaseInstaller = "https://github.com/$Owner/$Repo/releases/latest/download/$BinaryName-installer.ps1"
$RawBase = "https://raw.githubusercontent.com/$Owner/$Repo/main"
$PagesUrl = "https://$Owner.github.io/$Repo/"
$AllAgents = @("claude-code", "claude-desktop", "cursor", "windsurf", "continue", "codex", "hermes", "ollama")
$ProjectDir = (Get-Location).Path

function Log($msg)      { Write-Host "[install] " -ForegroundColor Blue -NoNewline; Write-Host $msg }
function Success($msg)  { Write-Host "[install] " -ForegroundColor Green -NoNewline; Write-Host $msg }
function Warn($msg)     { Write-Host "[install] " -ForegroundColor Yellow -NoNewline; Write-Host $msg }
function Fail($msg)     { Write-Host "[install] " -ForegroundColor Red -NoNewline; Write-Host $msg }

function Print-Help {
    Write-Host @"
semanticastindexer installer

Usage (one-liners; the scriptblock form is how flags pass through irm):
  powershell -c "irm ${PagesUrl}install.ps1 | iex"
  powershell -c "& ([scriptblock]::Create((irm ${PagesUrl}install.ps1))) -Platform claude-code"
  powershell -c "& ([scriptblock]::Create((irm ${PagesUrl}install.ps1))) -All -Write"

By default the binary is installed, then you are asked which coding agent(s) to connect.

Agent selection (skip the prompt):
  -Platform <id>       Connect one client non-interactively
  -All                 Connect every supported client
  -NonInteractive      Don't prompt; just install the binary + print a generic MCP block

Supported platform ids:
  claude-code  claude-desktop  cursor  windsurf  continue  codex  hermes  ollama  generic

Other options:
  -Collection <name>   Collection name (default: source_code)
  -Embedder <id>       ort | ollama (default: ort; the 'ollama' client forces ollama)
  -Write               Merge config into the client's file (JSON clients only; best-effort, backs up)
  -SkipBinary          Don't install the binary (just emit config)
  -Help                Show this help
"@
}

# --- Install the prebuilt binary via the cargo-dist release installer ---
function Install-Binary {
    if ($SkipBinary) {
        Warn "Skipping binary install (-SkipBinary)."
        return
    }
    Log "Downloading the latest prebuilt $BinaryName binary..."
    # Child process, like `curl | sh` in install.sh: the release installer can't
    # take our script down with an `exit`.
    powershell -NoProfile -ExecutionPolicy Bypass -Command "irm '$ReleaseInstaller' | iex"
    if ($LASTEXITCODE -ne 0) {
        Fail "Could not run the release installer."
        Fail "No release yet? Build from source instead - see $RawBase/docs/install.md"
        exit 1
    }
    Success "Binary installed."
}

# Resolve the absolute path to the installed binary: PATH lookup first, then the
# cargo-dist install dirs (%CARGO_HOME%\bin, ~\.cargo\bin, ~\.local\bin). If nothing
# exists on disk, warn and default to the ~\.cargo\bin path.
function Resolve-Binary {
    $candidates = @()
    $onPath = Get-Command $BinaryName -ErrorAction SilentlyContinue
    if ($onPath) { $candidates += $onPath.Source }
    if ($env:CARGO_HOME) { $candidates += (Join-Path $env:CARGO_HOME "bin\$BinaryName.exe") }
    $candidates += (Join-Path $HOME ".cargo\bin\$BinaryName.exe")
    $candidates += (Join-Path $HOME ".local\bin\$BinaryName.exe")
    foreach ($c in $candidates) {
        if ($c -and (Test-Path $c)) { return $c }
    }
    # Nothing found on disk — warn instead of silently returning a dead path (the MCP
    # config would point at a binary that does not exist).
    Warn "could not locate the installed binary; defaulting to ~\.cargo\bin\$BinaryName.exe"
    return (Join-Path $HOME ".cargo\bin\$BinaryName.exe")
}

# --- Config snippet builders ---
function Json-Escape($s) { return $s.Replace('\', '\\').Replace('"', '\"') }

function Json-Snippet($bin, $emb) {
    return @"
{
  "mcpServers": {
    "$ServerName": {
      "command": "$(Json-Escape $bin)",
      "args": ["mcp", "--backend", "duckdb", "--embedder", "$emb", "--collection", "$Collection"],
      "cwd": "$(Json-Escape $ProjectDir)"
    }
  }
}
"@
}

function Toml-Snippet($bin, $emb) {
    return @"
[mcp_servers.$ServerName]
command = '$bin'
args = ["mcp", "--backend", "duckdb", "--embedder", "$emb", "--collection", "$Collection"]
cwd = '$ProjectDir'
"@
}

function Yaml-Snippet($bin, $emb) {
    return @"
mcpServers:
  - name: $ServerName
    command: "$(Json-Escape $bin)"
    args: ["mcp", "--backend", "duckdb", "--embedder", "$emb", "--collection", "$Collection"]
    cwd: "$(Json-Escape $ProjectDir)"
"@
}

function Print-Block($where, $content) {
    Write-Host ""
    Write-Host "Add this to ${where}:" -ForegroundColor White
    Write-Host "----------------------------------------------------------------------"
    Write-Host $content
    Write-Host "----------------------------------------------------------------------"
}

# Best-effort merge of the MCP server entry into an existing JSON config; on a
# broken/missing file it starts fresh (after a .bak backup), like install.sh.
function Write-JsonConfig($target, $bin, $emb) {
    $dir = Split-Path -Parent $target
    if ($dir -and -not (Test-Path $dir)) { New-Item -ItemType Directory -Path $dir -Force | Out-Null }
    $cfg = $null
    if (Test-Path $target) {
        Copy-Item $target "$target.bak" -Force
        Log "Backed up existing config to $target.bak"
        try { $cfg = Get-Content $target -Raw | ConvertFrom-Json } catch { $cfg = $null }
    }
    if (-not $cfg) { $cfg = [pscustomobject]@{} }
    if (-not $cfg.PSObject.Properties['mcpServers']) {
        $cfg | Add-Member -NotePropertyName mcpServers -NotePropertyValue ([pscustomobject]@{})
    }
    $server = [pscustomobject]@{
        command = $bin
        args    = @("mcp", "--backend", "duckdb", "--embedder", $emb, "--collection", $Collection)
        cwd     = $ProjectDir
    }
    if ($cfg.mcpServers.PSObject.Properties[$ServerName]) {
        $cfg.mcpServers.$ServerName = $server
    } else {
        $cfg.mcpServers | Add-Member -NotePropertyName $ServerName -NotePropertyValue $server
    }
    $cfg | ConvertTo-Json -Depth 10 | Set-Content $target -Encoding UTF8
    Success "Wrote $target"
}

function Install-ClaudeSkill {
    $skillDir = Join-Path $HOME ".claude\skills\semantic-code-search-mcp"
    if (-not (Test-Path $skillDir)) { New-Item -ItemType Directory -Path $skillDir -Force | Out-Null }
    try {
        Invoke-WebRequest -UseBasicParsing -Uri "$RawBase/mcp-setup/SKILL.md" -OutFile (Join-Path $skillDir "SKILL.md")
        Success "Installed skill -> $skillDir\SKILL.md"
    } catch {
        Warn "Could not download SKILL.md (offline?). Skipping skill file."
    }
}

# Wire up one client.
function Configure-Platform($id, $bin) {
    $emb = $Embedder
    if ($id -eq "ollama") { $emb = "ollama" }
    Write-Host ""
    Write-Host "* $id" -ForegroundColor White
    switch ($id) {
        "claude-code" {
            Install-ClaudeSkill
            $target = Join-Path $ProjectDir ".mcp.json"
            if ($Write) { Write-JsonConfig $target $bin $emb }
            else { Print-Block "$target (in the project you want to search)" (Json-Snippet $bin $emb) }
        }
        "claude-desktop" {
            $target = Join-Path $env:APPDATA "Claude\claude_desktop_config.json"
            if ($Write) { Write-JsonConfig $target $bin $emb }
            else { Print-Block $target (Json-Snippet $bin $emb) }
        }
        "cursor" {
            $target = Join-Path $HOME ".cursor\mcp.json"
            if ($Write) { Write-JsonConfig $target $bin $emb }
            else { Print-Block $target (Json-Snippet $bin $emb) }
        }
        "windsurf" {
            $target = Join-Path $HOME ".codeium\windsurf\mcp_config.json"
            if ($Write) { Write-JsonConfig $target $bin $emb }
            else { Print-Block $target (Json-Snippet $bin $emb) }
        }
        "continue" {
            Print-Block (Join-Path $HOME ".continue\config.yaml") (Yaml-Snippet $bin $emb)
            if ($Write) { Warn "-Write supports JSON clients only; paste the YAML above into Continue's config." }
        }
        "codex" {
            Print-Block (Join-Path $HOME ".codex\config.toml") (Toml-Snippet $bin $emb)
            if ($Write) { Warn "-Write supports JSON clients only; paste the TOML above into ~\.codex\config.toml." }
        }
        "hermes" {
            Warn "Hermes config location is client-specific - paste this generic MCP block into its MCP config."
            Print-Block "your Hermes MCP config" (Json-Snippet $bin $emb)
        }
        "ollama" {
            Warn "Ollama is the embedding backend, not an MCP client. Make sure it is running:"
            Write-Host "    ollama serve"
            Write-Host "    ollama pull nomic-embed-text"
            Print-Block "your MCP client config (uses --embedder ollama)" (Json-Snippet $bin $emb)
        }
        "generic" {
            Print-Block "your MCP client config" (Json-Snippet $bin $emb)
        }
        default {
            Warn "Unknown platform '$id' - skipping."
        }
    }
}

# Interactive multi-select. Returns the chosen platform ids (empty = skip).
function Prompt-Agents {
    Write-Host ""
    Write-Host "Which coding agent(s) should I connect? (the binary works as a CLI regardless)"
    Write-Host ""
    Write-Host "   1) Claude Code       4) Windsurf        7) Hermes"
    Write-Host "   2) Claude Desktop    5) Continue.dev    8) Ollama"
    Write-Host "   3) Cursor            6) Codex CLI       9) Generic / manual"
    Write-Host ""
    $choice = Read-Host "Enter numbers (e.g. 1 3), 'all', or press Enter to skip"

    switch -Regex ($choice.Trim()) {
        '^$|^(n|no|none|skip)$' { return @() }
        '^(all|a)$'             { return $AllAgents }
    }

    $map = @{ "1" = "claude-code"; "2" = "claude-desktop"; "3" = "cursor"; "4" = "windsurf";
              "5" = "continue"; "6" = "codex"; "7" = "hermes"; "8" = "ollama"; "9" = "generic" }
    $out = @()
    foreach ($tok in ($choice -split '\s+' | Where-Object { $_ })) {
        if ($map.ContainsKey($tok)) { $out += $map[$tok] }
        else { Write-Host "Ignoring unknown choice: $tok" }
    }
    return $out
}

# --- Main ---
function Main {
    if ($Help) { Print-Help; return }
    Write-Host ""
    Write-Host "semanticastindexer installer" -ForegroundColor White
    Write-Host ""
    Install-Binary
    $bin = Resolve-Binary
    Log "Binary: $bin"

    # Decide which clients to wire up.
    $targets = @()
    if ($Platform) {
        $targets = @($Platform)
    } elseif ($All) {
        $targets = $AllAgents
    } elseif ($NonInteractive -or [Console]::IsInputRedirected -or -not [Environment]::UserInteractive) {
        $targets = @("generic")
    } else {
        $targets = Prompt-Agents
    }

    if (-not $targets -or $targets.Count -eq 0) {
        Log "No client selected. The binary is installed; re-run with -Platform <id> or -All to wire one up."
    } else {
        foreach ($id in $targets) { Configure-Platform $id $bin }
    }

    Write-Host ""
    Success "Done. Next steps:"
    # Human-facing commands use the bare name — the absolute path only belongs in the
    # MCP config snippets. If the binary is not on this session's PATH yet, say so.
    $step = 1
    if (-not (Get-Command $BinaryName -ErrorAction SilentlyContinue)) {
        $binDir = Split-Path $bin
        Write-Host "  $step. Open a new terminal so '$BinaryName' is on your PATH ($binDir)"
        $step++
    }
    Write-Host "  $step. cd into the project you want to search"
    $step++
    Write-Host "  $step. $BinaryName --root src --ext ts,tsx --dry-run   # preview what gets indexed"
    $step++
    Write-Host "  $step. $BinaryName --root src --ext ts,tsx             # index it"
    $step++
    Write-Host "  $step. Restart your client so it picks up the MCP server"
    Write-Host ""
    Write-Host "  Docs: $PagesUrl"
}

Main
