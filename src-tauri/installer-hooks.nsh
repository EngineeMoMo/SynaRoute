; SynaRoute 的 NSIS 安装器钩子
;
; 挂在 tauri.conf.json 的 bundle.windows.nsis.installerHooks。
; 修的是两条实测出来的卸载缺陷（判据见 docs/14）：
;   1. 卸载不还原客户端配置 → 卸载后 claude/codex 仍指向 127.0.0.1:47100（已无人监听）
;   2. 内置的「删除应用数据」删错目录 → 勾了也删不掉 config.json / secrets.enc
;
; ============================================================================
; 「这是真卸载吗」—— 本文件所有动作的总闸，两个宏都必须过
; ============================================================================
;
; Section Uninstall 有三条进入路径，只有第一条才是用户真的要卸载：
;
;   a) 用户从「应用和功能」/开始菜单卸载
;      → 跑的是注册表里的 UninstallString，值为裸 `"$INSTDIR\uninstall.exe"`
;        （installer.nsi:679），**不带任何参数**
;   b) 应用内自动更新（tauri-plugin-updater 传 /UPDATE，updater.rs:812）
;      → 实测 installer.nsi:314-316：`${If} $UpdateMode = 1` 直接 `Goto reinst_done`，
;        **根本不执行 Section Uninstall**。所以 $UpdateMode 在卸载器里其实恒为 0，
;        单靠它做守卫等于没有守卫。
;   c) 用户双击新版 setup.exe 手动升级（也含降级，ALLOWDOWNGRADES=true）
;      → 默认选中「先卸载旧版」，走 reinst_uninstall 拉起旧 uninstall.exe，
;        命令行**无条件追加 `_?=$4`**（installer.nsi:354），但不带 /UPDATE 也不带 /P
;
; 于是判据是 `_?=` 而不是 /UPDATE：有 `_?=` 说明是安装器在卸旧版（路径 c），
; 用户的心理模型是「我在升级」，此时既不该回滚他的客户端配置、更不该删他的密钥库。
;
; **这条守卫是本文件风险最高的部分**。漏了它，用户双击新版安装包一路默认下一步，
; 就会：客户端配置被回滚到几个月前的快照且唯一备份被删；若他顺手勾了升级向导里那个
; 「删除应用数据」（在升级语境下很容易被理解成"清理旧版残留"），全部 API Key 与
; secrets.enc 当场永久消失。
;
; $UpdateMode 的判断保留：它现在是纵深防御而非主判据，将来 tauri 模板若改成
; 「更新也走卸载节」，这条能兜住。

; ---- 卸载前：还原三端客户端配置 + 摘除 MCP 注册 ----
;
; 必须放 PRE 而不是 POST：POST 执行时 $INSTDIR\synaroute.exe 已被删除，无从调用。
; （已核对 tauri 生成的 installer.nsi：PREUNINSTALL 宏在 `Delete "$INSTDIR\..."` 之前）
;
; 寄存器用 $R5~$R7：Section Uninstall 的内置代码复用 $0/$1 收 nsExec 与插件返回值，
; 借用会互相踩。
!macro NSIS_HOOK_PREUNINSTALL
  ClearErrors
  ${GetOptions} $CMDLINE "_?=" $R5
  ${If} ${Errors}                                   ; 无 _?= → 用户真的在卸载
  ${AndIf} $UpdateMode <> 1                         ; 纵深防御，见文件头
  ${AndIf} ${FileExists} "$INSTDIR\synaroute.exe"
    ; /TIMEOUT 是硬要求：还原是纯文件操作、正常远小于 1s，但本项目历史上被静默安装
    ; 卡死过两次，绝不能让卸载器挂在一个子进程上。超时就放弃还原、继续卸载 ——
    ; 残留配置可以手动改，卸载卡死用户只能去任务管理器。
    nsExec::ExecToStack /TIMEOUT=15000 '"$INSTDIR\synaroute.exe" --uninstall-cleanup'
    Pop $R6
    Pop $R7
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
  ; 三重守卫，缺一不可：
  ;   - 复选框：用户没勾就绝不删（与内置块同一条件，逐字对齐）
  ;   - _?= ：安装器在卸旧版（手动升级）时绝不删，见文件头
  ;   - $UpdateMode：纵深防御
  ClearErrors
  ${GetOptions} $CMDLINE "_?=" $R5
  ${If} $DeleteAppDataCheckboxState = 1
  ${AndIf} ${Errors}
  ${AndIf} $UpdateMode <> 1
    SetShellVarContext current

    ; 删前留一份逃生副本再删（项目硬规则：改/删数据前必先备份，可回滚）。
    ; secrets.enc 由 DPAPI 绑当前 Windows 账户加密、盘上无第二份、跨机也解不出，
    ; RMDir /r 下去就是永久丢失全部 API Key。这里把两个不可再生的文件挪到
    ; 「已删除」目录，用户误勾时还有救；纯缓存/日志不留。
    ; 用 Rename 而非 CopyFiles：同盘移动是原子的，且不会因为文件大而拖慢卸载。
    ${If} ${FileExists} "$APPDATA\SynaRoute\secrets.enc"
    ${OrIf} ${FileExists} "$APPDATA\SynaRoute\config.json"
      CreateDirectory "$APPDATA\SynaRoute.removed"
      Rename "$APPDATA\SynaRoute\secrets.enc" "$APPDATA\SynaRoute.removed\secrets.enc"
      Rename "$APPDATA\SynaRoute\config.json" "$APPDATA\SynaRoute.removed\config.json"
    ${EndIf}

    RMDir /r "$APPDATA\SynaRoute"

    ; Windows 上运行日志优先落 exe 同级 logs\（store.rs 的 resolve_default_log_dir
    ; 刻意这么设，为躲 MSIX 虚拟化），currentUser 安装时即 $INSTDIR\logs。
    ; 开了请求日志时里面是**明文对话正文**，用户勾了「删除应用数据」却留着它，
    ; 是比留配置更糟的隐私问题。内置卸载节只对 $INSTDIR 做非递归 RMDir，
    ; 有 logs\ 子目录必然失败，故这里补删。
    RMDir /r "$INSTDIR\logs"
    RMDir "$INSTDIR"
  ${EndIf}
!macroend
