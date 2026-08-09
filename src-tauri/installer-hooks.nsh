; SynaRoute 的 NSIS 安装器钩子
;
; 挂在 tauri.conf.json 的 bundle.windows.nsis.installerHooks。
; 修的是两条实测出来的卸载缺陷（判据见 docs/14）：
;   1. 卸载不还原客户端配置 → 卸载后 claude/codex 仍指向 127.0.0.1:47100（已无人监听）
;   2. 内置的「删除应用数据」删错目录 → 勾了也删不掉 config.json / secrets.enc

; ---- 卸载前：还原三端客户端配置 ----
;
; 必须放 PRE 而不是 POST：POST 执行时 $INSTDIR\synaroute.exe 已被删除，无从调用。
; （已核对 tauri 生成的 installer.nsi：PREUNINSTALL 宏在 `Delete "$INSTDIR\..."` 之前）
!macro NSIS_HOOK_PREUNINSTALL
  ${If} ${FileExists} "$INSTDIR\synaroute.exe"
    ; /TIMEOUT 是硬要求：还原是纯文件操作、正常远小于 1s，但本项目历史上被静默安装
    ; 卡死过两次，绝不能让卸载器挂在一个子进程上。超时就放弃还原、继续卸载 ——
    ; 残留配置可以手动改，卸载卡死用户只能去任务管理器。
    nsExec::ExecToStack /TIMEOUT=15000 '"$INSTDIR\synaroute.exe" --uninstall-cleanup'
    Pop $0
    Pop $1
  ${EndIf}
!macroend

; ---- 卸载后：删真正的数据目录 ----
;
; 内置卸载节删的是 $APPDATA\${BUNDLEID}（= com.synaroute.app），而应用实际写的是
; $APPDATA\SynaRoute（store.rs 用 dirs::data_dir().join("SynaRoute")）。两者从来对不上，
; 导致「删除应用数据」复选框勾了也只删掉 WebView2 缓存，config.json 与 secrets.enc
; 一个字节没动 —— 用户以为密钥清了，其实还在盘上。这里补删正确路径。
; 内置那两行不动：它们删的 WebView2 缓存本就该删。
!macro NSIS_HOOK_POSTUNINSTALL
  ; $UpdateMode <> 1 这个守卫**绝不能省**：应用内更新走的是同一套卸载流程，
  ; 漏了它会在每次自动更新时把用户的全部 Key 与密钥库删光。这是本文件风险最高的一行。
  ; 条件与内置块逐字对齐（installer.nsi 里是 `= 1` 与 `<> 1`）。
  ${If} $DeleteAppDataCheckboxState = 1
  ${AndIf} $UpdateMode <> 1
    SetShellVarContext current
    RMDir /r "$APPDATA\SynaRoute"
  ${EndIf}
!macroend
