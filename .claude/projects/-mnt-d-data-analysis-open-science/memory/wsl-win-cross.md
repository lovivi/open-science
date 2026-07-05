---
name: wsl-win-cross
description: WSL2 可以通过 Windows 工具链构建 Tauri 桌面 exe
metadata:
  type: reference
---

从 WSL2 内可以直接调用 Windows 侧的可执行文件、工具链和编译器，前提是 Windows 侧已安装所需工具。

## Open Science Tauri Windows 构建要点

### Windows 侧需要安装
1. **Node ≥20** — 已安装 (v23.9.0 at C:\Users\song\Tools\node)
2. **pnpm 9** — 已通过 npm global install 安装
3. **Rust MSVC target** — 已通过 rustup-init 安装 (1.96.1)
4. **Visual Studio 2022 Build Tools** — 需要安装，含 VCTools 组件
   - 安装命令: C:\Program Files (x86)\Microsoft Visual Studio\Installer\vs_installer.exe modify --installPath "C:\VSBT2022" --add Microsoft.VisualStudio.Component.VC.Tools.x86.x64
   - 或重新运行: C:\Users\song\AppData\Local\Temp\vs_bt.exe --installPath "C:\VSBT2022" --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended

### 构建命令（从 WSL 调用 Windows）
```bash
# 方式1: 通过 cmd.exe 调用
cmd.exe /c "C:\Windows\Temp\build.cmd"

# 方式2: 在 Windows PowerShell 直接运行
cd \\wsl$\Ubuntu\mnt\d\data_analysis\open-science
pnpm install --frozen-lockfile
pnpm tauri build
```

### 构建脚本位于 Windows Temp
- `C:\Windows\Temp\build.cmd` — 自动完成 pnpm install → fetch sidecars → tauri build

### 产物路径
`apps/desktop/src-tauri/target/release/bundle/` 下的 .exe / .msi

### 注意事项
- WSL 的非交互环境下 VS Build Tools 的 `--quiet` 安装可能不完整，需要交互式安装
- 第一次安装 VS Build Tools 需要管理员权限
- pnpm 在 Windows 侧通过 `npm install -g pnpm@9` 安装
- Rust 在 Windows 侧通过 `C:\Windows\Temp\rustup-init.exe -y --default-host x86_64-pc-windows-msvc` 安装
