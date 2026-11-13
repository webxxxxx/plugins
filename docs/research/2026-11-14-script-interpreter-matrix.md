# 脚本 payload 的解释器矩阵：五语言 × 三目标

`.spk` 里真正会被解释器执行的，只有**脚本类 payload**。native 可执行文件（ELF / PE）直接跑，不经过解释器；一旦 payload 不是 native，manifest 就必须声明 `interpreter`，否则验包阶段直接拒（`plugin-manager/src/package.rs:511-544`）。

这份记录回答一个问题：`sh` / `bat` / `ps1` / `python` / `node` 五种语言，在三个目标平台上分别该写什么 `interpreter.name` 与 `interpreter.args`，以及哪些组合根本不该进模板。

> **路径约定**：下文 `$PM` = `D:\learn\harmess_workspace\plugin-manager`，`$SPEC` = `$PM\docs\specs\2026-11-12-核心层需求最终版.md`。所有 `$PM:行号` 引用的都是 2026-11-13 17:10 那版工作副本。

## 0. 一句话答案

三个目标之间的真正差别**不在 libc，而在"宿主用户态里到底装了什么"**：

- 两个 `*-linux-musl` 目标只保证 `/bin/sh`（musl 发行版通常是 BusyBox ash）；`python` / `node` / `pwsh` 在这类机器上是"另外装的东西"，嵌入式固件上基本等于不存在。
- `x86_64-pc-windows-msvc` 保证 `cmd.exe` 与 Windows PowerShell 5.1；`python` / `node` / `pwsh` 同样是选装。
- `.bat` 只能靠 `cmd.exe /C`，`.ps1` 只能靠 `-File`，`interpreter.args` 必须排在脚本路径**之前**——这三条锁死了所有可行配方。

因此一期可落地的配方只有两个层次：**`sh`（两个 musl 目标）与 `cmd /C` / `powershell -File`（Windows）是"自带解释器"层级；`python` / `node` / `pwsh` 一律是"宿主已装则可用"层级。**

## 1. 判定口径

### 1.1 三个取值

| 取值 | 定义 |
|---|---|
| **可行** | 目标平台**自带**该可执行文件，manifest 写对即可安装成功、可被监督运行 |
| **有条件可行** | 目标不自带，但该目标的部署形态允许、且现实中常见安装该运行时；模板可用，但必须在文档里写明前置依赖 |
| **不可行** | 该可执行文件在目标上不存在，或该目标的部署形态没有合理的安装路径；下游必须换实现方式（改 native，或换语言） |

「不可行」里混了两类，第 5 节把它们分开：**物理不可能**（3 格）与**一期不接受**（3 格）。

### 1.2 决定性前提：解释器解析在宿主用户态，不在 daemon 的静态二进制里

`svcd` 是 musl 静态链接（`$SPEC:236-237`，实测 504,544 B），但 `interpreter` 解析出来的路径是交给 `Command::new()` 去 spawn 的（`$PM/src/slot.rs:928-942`）。也就是说：

- 静态链接只说明 **daemon 自己不依赖 libc**，完全不说明宿主上有没有 `python3`。
- 所以 `x86_64-unknown-linux-musl` 这个三元组描述的是"**一个 musl 静态二进制能跑的 x86_64 Linux**"，这个宿主可以是 Debian（glibc 用户态 + 现成 `python3`），也可以是 Alpine（BusyBox + 无 python）。**同一个三元组，两种完全不同的用户态。**
- `aarch64-unknown-linux-musl` 在交付里对应的就是嵌入式那一栏（≤ 2,000,000 B、不编 WebView、TLS 不链；目标清单本身还写着"OpenWrt 版本 / libc / 内核 / 存储布局：雾区"，`$SPEC:233-239`、`$SPEC:258`）。

这条前提是整张表的地基：**矩阵量的是"宿主有什么"，不是"daemon 链了什么"。**

### 1.3 三个目标 = 三种部署形态

| manifest.`platform` | 交付里它代表什么 | 自带 | 常见选装 |
|---|---|---|---|
| `x86_64-unknown-linux-musl` | 桌面/服务器 Linux（静态单文件交付） | `sh` | `python3`、`node`、`pwsh` |
| `aarch64-unknown-linux-musl` | 嵌入式 ARM 设备（路由器/NAS/SBC） | `sh`（BusyBox） | 基本没有 |
| `x86_64-pc-windows-msvc` | Windows 11 x86_64 | `cmd`、`powershell` | `python`、`py`、`node`、`pwsh` |

`platform` 与构建时的 `TARGET` **精确相等**（`$SPEC:132`，`$PM/src/package.rs:318-324`），一个包只服务一个平台；`interpreter` 是"这个包在这台机器上能不能跑"的第二道闸。

## 2. 解析器实际做了什么

文档（`$SPEC:158-166`）是意图，`$PM/src/interpreter.rs` 是行为。下面十条直接决定矩阵：

1. **名字只认裸名且字符集很窄**：`[A-Za-z0-9._-]`，空串、路径、空格全部拒（`interpreter.rs:104-109`，测试 `:472-479`）。所以文档里的 `py -3` 写法**不能**塞进 `name`，必须 `name: "py"` + `args: ["-3"]`。
2. **Windows 搜索是 PATH × PATHEXT 拼候选**（`interpreter.rs:129-151`，PATHEXT 缺省 `.COM;.EXE;.BAT;.CMD`），但**过滤器只放 `.exe` / `.com`**（`:305-315`）。所以 `.cmd` / `.bat` shim 会拼出候选、又被扩展名检查丢掉——这正是"Windows 只解析 .exe/.com"的实现落点。
3. **Unix 搜索不做任何后缀猜测**：只试 `目录/name`（`interpreter.rs:152-158`）。**`python` 和 `python3` 是两个不同的名字，没有回退。**
4. **版本探测只看 `version` 是否非空**：空串或省略 = 只查存在、根本不 spawn 探针（`interpreter.rs:62-70`、`:577-597` 的测试专门钉住"空 version 不跑探针"）。
5. **探针命令按 `file_stem` 分派，只有三个特例**（`interpreter.rs:341-349`）：
   - `powershell` → `-NoProfile -Command $PSVersionTable.PSVersion.ToString()`
   - `cmd` → `/C ver`
   - **其余全部** → `--version`（`pwsh`、`python`、`python3`、`py`、`node`、`sh`、`bash` 都走这条）
6. **版本解析取输出里第一个 `x.y[.z...]`，至少两段**（`interpreter.rs:169-204`）：`v20.11.1` → `20.11.1`，`GNU bash, version 5.2.15(1)-release` → `5.2.15`，`2026` → 解析失败。
7. **约束语法只有 `>= <= > < =`，逗号表示 AND**；裸 `3.10`、`~3.10`、`^3.10` 全部非法（`interpreter.rs:246-277`，测试 `:452-467`）。
8. **探针硬超时 5000 ms**，超时即失败（`interpreter.rs:41-44`、`:378-388`）。
9. **运行时命令行 = `interp` + `interp.args` + `[脚本绝对路径]` + `exec.args`**（`slot.rs:928-942`；脚本路径由 `version_dir.join(exec.path)` 得到，`$PM/src/plugin.rs:627`）。**一个 manifest 只有一个 `interpreter`，`exec` 与全部 `lifecycle` 共用它**（`$SPEC:164`，`plugin.rs:726-730`、`:788-792`）。
10. **`exec.ready` 缺省是 `notify`**（`plugin.rs:631-635`），而 `ready=exec` 不允许声明 `capabilities`（`package.rs:572-580`）。脚本 payload 不会说 MCP 协议，**必须显式写 `"ready": "exec"`**。

另有一条只影响观感的实现细节：`check_path` 禁止反斜杠（`package.rs:866-876`），所以 `exec.path` / `lifecycle.*.path` 一律用 `/`；Windows 上拼出来的最终路径是 `C:\ProgramData\svcd\plugins\<id>\<ver>\payload/clock.bat` 这种混合分隔符。实测 `cmd /C` 对这种混合路径正常（见 §6）。

## 3. 可达性矩阵

| 语言 | `x86_64-unknown-linux-musl` | `aarch64-unknown-linux-musl` | `x86_64-pc-windows-msvc` |
|---|---|---|---|
| **`sh`** | **可行** — 任何 Linux 都有 `/bin/sh`（musl 上是 BusyBox ash），裸名 `sh` 一定解析得到 | **可行** — 同左；这是嵌入式上唯一稳的脚本形态 | **不可行** — Windows 没有 `sh`；`bash.exe` 是 WSL 的 Linux 启动器，Git 的 `sh.exe` 不是系统组件 |
| **`bat`** | **不可行** — 批处理是 `cmd.exe` 的语法，Linux 上没有 `cmd` 可解析 | **不可行** — 同左 | **可行** — 唯一配方 `name=cmd` + `args=["/C"]`；`.bat` 由 cmd 自己解释，不派生子进程 |
| **`ps1`** | **有条件可行** — 需宿主装 PowerShell 7，且裸名是 `pwsh`（不是 `powershell`） | **不可行** — 嵌入式不装 .NET 运行时，`pwsh` 不存在 | **可行** — Windows 自带 `powershell`（5.1），`-NoProfile -ExecutionPolicy Bypass -File` |
| **`python`** | **有条件可行** — 裸名用 `python3`；`python` 在新版 Debian/Ubuntu 上常常不存在 | **不可行** — 嵌入式固件不提供 Python 运行时 | **有条件可行** — 只能 `python` 或 `py`；**`python3` 不可用**（见 §6.1） |
| **`node`** | **有条件可行** — 裸名 `node`，探针 `--version` 输出 `v20.11.1` 可解析 | **不可行** — 嵌入式不提供 V8 运行时 | **有条件可行** — Windows 上是 `node.exe`，裸名 `node` |

统计：可行 4 格、有条件可行 5 格、不可行 6 格。

## 4. 可直接抄的 manifest 片段

以下片段是**源清单**形态（人写的 `manifest.json`，没有 `files[]`——`files[]` 只能由打包器算）。省略了 `name` / `version` / `desc` / `protocol` 这些与本题无关的字段；`schema` 必须是 `2`，否则 `interpreter` / `lifecycle` 直接拒（`package.rs:551-556`）。

模板里还有两条纪律：`exec.path` 与 `lifecycle.*.path` 指向的文件都必须在包里；`hooks` 指向的脚本需要可执行位，而解释器执行的 payload **不需要**（`$PM/src/bundle.rs:589-601` 会替 exec 补 `0755`，lifecycle 文件保持原 mode）。

### 4.1 `sh` @ 两个 musl 目标（首选形态）

```json
{
  "schema": 2,
  "platform": "x86_64-unknown-linux-musl",
  "interpreter": { "name": "sh" },
  "exec": { "path": "payload/clock.sh", "ready": "exec" },
  "lifecycle": {
    "start":  { "path": "payload/clock.sh", "args": ["start"] },
    "stop":   { "path": "payload/clock.sh", "args": ["stop"], "timeout": 10 },
    "status": { "path": "payload/clock.sh", "args": ["status"], "timeout": 5 }
  }
}
```

- **`interpreter.args` 留空**。要 fail-fast 才加 `["-eu"]`（`sh -eu <script>`），但 `-e` 会改变脚本语义，模板默认不要加。
- **`version` 一律省略**。这不是省事：`sh` 走 `--version` 探针，而 Debian 的 `sh` 是 dash，实测 `sh --version` 直接 `sh: 0: Illegal option --`（退出码 2），探针解析不到版本 → 安装被拒。详见 §6.2。
- `aarch64-unknown-linux-musl` 只需改 `platform`，其余一字不改。样例 `samples/services.d/clock.sh` 就是这个形状（声明 `start`/`status`，无 `stop`）。
- 如果脚本从顶部一路执行、不认 action 参数，就把 `lifecycle` 整块删掉，只留 `exec`——`lifecycle.start` 存在时会**替换** `exec` 作为被监督的前台进程（`$SPEC:164`）。

### 4.2 `bat` @ Windows（唯一配方）

```json
{
  "schema": 2,
  "platform": "x86_64-pc-windows-msvc",
  "interpreter": { "name": "cmd", "args": ["/C"] },
  "exec": { "path": "payload/clock.bat", "ready": "exec" },
  "lifecycle": {
    "start":  { "path": "payload/clock.bat", "args": ["start"] },
    "stop":   { "path": "payload/clock.bat", "args": ["stop"] },
    "status": { "path": "payload/clock.bat", "args": ["status"] }
  }
}
```

- `name` 必须是 `cmd`（解析到 `cmd.exe`）；写 `cmd.exe` 也能解析，但没必要。
- **`version` 建议省略**。`cmd` 的探针是 `/C ver`，输出 `Microsoft Windows [Version 10.0.26200.9168]` —— 解析出来的是 **Windows 构建号，不是 cmd 的版本**。真写约束只能写 `">=10"` 这种"顺便校验了 Windows 版本"的东西，语义会误导。
- 参数注入点在脚本里：`clock.bat:10` 的 `set "ACTION=%~1"`，action 由 `lifecycle.*.args` 或 `exec.args` 传进去，落在 `%~1`。`exit /b N` 会成为 cmd 的退出码。
- `.bat` 由 cmd **自己逐行解释**，不派生新的子进程，所以 Windows 作业对象杀进程树时不会漏掉批处理本体。

### 4.3 `ps1` @ Windows

```json
{
  "schema": 2,
  "platform": "x86_64-pc-windows-msvc",
  "interpreter": {
    "name": "powershell",
    "args": ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"],
    "version": ">=5.1"
  },
  "exec": { "path": "payload/svc.ps1", "ready": "exec" },
  "lifecycle": {
    "start":  { "path": "payload/svc.ps1", "args": ["start"] },
    "stop":   { "path": "payload/svc.ps1", "args": ["stop"], "timeout": 10 },
    "status": { "path": "payload/svc.ps1", "args": ["status"], "timeout": 5 }
  }
}
```

- 顺序是硬要求：**`-File` 必须是 `args` 的最后一个**，因为"`-File` 之后的所有内容都被当作脚本路径和脚本参数"（[about_PowerShell_exe](https://learn.microsoft.com/en-us/powershell/module/microsoft.powershell.core/about/about_powershell_exe)：*"**File** must be the last parameter in the command"*）。而框架恰好把脚本路径接在 `args` 后面，两者刚好对上。
- `-ExecutionPolicy Bypass` 必须在 `-File` **之前**；写在后面会被当成脚本参数。这一点本机实证过（§6.4）。
- `version: ">=5.1"` 是安全的：探针输出 `5.1.26100.9168`，`parse_version` 取到 `5.1.26100.9168`，`>=5.1` 通过。想支持 pwsh 就换成下面 §4.4 的名字，二者别混。
- 若安装根落在 UNC 路径上，`RemoteSigned` 会拦远程脚本——这时 `Bypass` 不是可选项而是必需项。

### 4.4 `ps1` @ `x86_64-unknown-linux-musl`

```json
{
  "schema": 2,
  "platform": "x86_64-unknown-linux-musl",
  "interpreter": { "name": "pwsh", "args": ["-NoProfile", "-File"], "version": ">=7.2" },
  "exec": { "path": "payload/svc.ps1", "ready": "exec" },
  "lifecycle": {
    "start":  { "path": "payload/svc.ps1", "args": ["start"] },
    "status": { "path": "payload/svc.ps1", "args": ["status"], "timeout": 5 }
  }
}
```

- Linux 上裸名是 **`pwsh`**，没有 `powershell`。"内置 `powershell`、选装 `pwsh`" 的分裂只在 Windows 上存在。
- **不要写 `-ExecutionPolicy`**：`-ExecutionPolicy` 仅对 Windows 有效，非 Windows 平台上"参数和值都被忽略"（[about_pwsh](https://learn.microsoft.com/en-us/powershell/module/microsoft.powershell.core/about/about_pwsh)）。写了是噪音。
- 探针是 `pwsh --version` → `PowerShell 7.6.7` → `7.6.7`，`>=7.2` 通过。
- 这一格是"有条件"：宿主必须先装 PowerShell 7（`/opt/microsoft/powershell/7/pwsh` 并链进 PATH）。`platform` 相同不代表装机量相同，模板文档必须写清这条前置依赖。

### 4.5 `python` @ `x86_64-unknown-linux-musl`

```json
{
  "schema": 2,
  "platform": "x86_64-unknown-linux-musl",
  "interpreter": { "name": "python3", "args": ["-u"], "version": ">=3.8" },
  "exec": { "path": "payload/agent.py", "ready": "exec" },
  "lifecycle": {
    "start":  { "path": "payload/agent.py", "args": ["start"] },
    "status": { "path": "payload/agent.py", "args": ["status"], "timeout": 5 }
  }
}
```

- **裸名用 `python3`**。新版 Debian/Ubuntu 默认不提供 `python`（只有 `python3`），而 Unix 侧不做后缀或别名回退（`interpreter.rs:152-158`）。
- **`args: ["-u"]` 建议别省**：被监督进程的 stdout 不是 TTY，Python 会块缓冲，日志会成批延迟出现；`ready=notify` 的场景下缓冲会直接把协议帧卡住。
- 探针 `python3 --version` → `Python 3.11.2`（stdout），`>=3.8` 通过。
- 仓库里的 `samples/wireguard/manifest.json:60-63` 就是这个配方（`python3` + `>=3.8`）。注意该样本同时带 `hooks`（`:47-50`），而嵌入式**默认拒绝带 hooks 的包**（`$SPEC:155`）——那个样本在 aarch64 上会因 hooks 被拒，与解释器无关。

### 4.6 `python` @ Windows

```json
{
  "schema": 2,
  "platform": "x86_64-pc-windows-msvc",
  "interpreter": { "name": "python", "args": ["-u"], "version": ">=3.10" },
  "exec": { "path": "payload/agent.py", "ready": "exec" }
}
```

要指定大版本就用 `py` 启动器，把版本选择放进 `args`：

```json
{ "name": "py", "args": ["-3", "-u"], "version": ">=3.10" }
```

- **绝对不要写 `python3`**。这是整张表最危险的一格，理由在 §6.1。
- `python` 与 `py` 都是官方认可入口（[Using Python on Windows](https://docs.python.org/3/using/windows.html)：推荐 `python`；需要多版本时用 `py`）。`py -3 …` 正好符合"`args` 排在脚本路径之前"。
- 探针 `python --version` 与 `py --version` 都输出 `Python 3.10.2`（stdout），`parse_version` 取 `3.10.2`。

### 4.7 `node` @ 两个目标

```json
{
  "schema": 2,
  "platform": "x86_64-unknown-linux-musl",
  "interpreter": { "name": "node", "version": ">=18" },
  "exec": { "path": "payload/svc.mjs", "ready": "exec" }
}
```

Windows 只把 `platform` 换成 `x86_64-pc-windows-msvc`，`interpreter` 一字不改（裸名 `node` 解析到 `node.exe`）。

- 裸名在两处都是 `node`。Node 的命令行签名是 `node [options] [V8 options] [<program-entry-point>] [--] [arguments]`（[Node CLI](https://nodejs.org/api/cli.html)），与"`args` 在脚本前"天然一致；需要运行时开关就写 `args: ["--enable-source-maps"]`。
- 探针 `node --version` → `v20.11.1` → `20.11.1`，`>=18` 通过。
- Node 没有 Python 那种 stdout 缓冲问题，不需要 `-u` 类开关。

## 5. 一期不支持

6 个不可行格分两类，混为一谈会让下游误判"以后能开"。

### 5.1 物理不可能（不要指望排期能解决）

| 格子 | 理由 |
|---|---|
| `sh` @ `x86_64-pc-windows-msvc` | Windows 没有 `sh`。`bash.exe` 是 WSL 的 Linux 发行版启动器（需要额外装发行版），Git 附带的 `sh.exe` 不是系统组件、不能作为发行依赖。要跨平台就写两份 payload，别指望一个 `.sh` 走遍三平台。 |
| `bat` @ 两个 musl 目标 | 批处理语法只有 `cmd.exe` 能执行。CreateProcess 层面对批处理的唯一正解就是"起命令解释器"（[CreateProcessW](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-createprocessw)：*"To run a batch file, you must start the command interpreter; set lpApplicationName to cmd.exe and set lpCommandLine to … /c plus the name of the batch file"*），而 Linux 上没有 `cmd` 这个名字可解析。改 native 或改 `sh`。 |

### 5.2 一期不接受（工程判定，不是技术不可能）

| 格子 | 理由 |
|---|---|
| `ps1` @ `aarch64-unknown-linux-musl` | `pwsh` 要拖进整个 .NET 运行时，与嵌入式"≤ 2 MB、不编 WebView、TLS 不链"的预算和形态直接冲突（`$SPEC:233-239`）。技术上能装，产品上不接受。 |
| `python` @ `aarch64-unknown-linux-musl` | 同上：嵌入式固件不提供 Python，也不该为插件在设备上装解释器。 |
| `node` @ `aarch64-unknown-linux-musl` | 同上：V8 运行时体积与嵌入式预算不相容。 |

一句话：**aarch64 嵌入式只有 `sh`。要在嵌入式上跑逻辑，要么写 `sh`，要么把逻辑做成 native 二进制打进去。**

## 6. 最容易翻车的坑

按"踩了之后症状有多隐蔽"排序。

### 6.1 Windows 上的 `python3` 是一个 0 字节存根，能通过解析、装上、然后在运行期 9009

这是唯一一个"验包通过、安装成功、启动才炸"的坑：

- 解析阶段：`python3` 在 PATH 上确实存在（`%LocalAppData%\Microsoft\WindowsApps\python3.exe`），`is_file()` 为真、扩展名 `.exe` 通过 `is_executable`（`interpreter.rs:305-315`）——**框架认为它解析成功**。
- 探测阶段：如果写了 `version`，`python3 --version` 直接 `exit=9009`、无输出，报"无法确定版本"；**如果省略 `version`（只校验存在），验包与安装都会通过**。
- 运行阶段：spawn 失败，服务永远起不来。

本机实测（Windows 11 26200 / 2026-11-14）：

```text
python3 --version -> exit=9009 out=[]            ← 0 字节的 WindowsApps 存根
python  --version -> exit=0    out=[Python 3.10.2]
py      --version -> exit=0    out=[Python 3.10.2]
```

根因是 Windows 上 Python 的历史包袱：官方从不把 `python3.exe` 当作正式入口。Python 官方文档的措辞很直白——`python3` 只是一个"用来接住 POSIX 习惯的别名"，"不打算被广泛使用或推荐"（[Using Python on Windows](https://docs.python.org/3/using/windows.html)：*"A `python3` command is also included that mimics the `python` command. It is intended to catch accidental uses of the typical POSIX command on Windows, but is not meant to be widely used or recommended."*）。

**推论：模板的 Windows python 配方里 `name` 只能是 `python` 或 `py`；并且 Windows 上千万别靠"省略 version 只查存在"来省事——那条路径会把存根放过去。**

### 6.2 给 `sh` 加 `version` 会在 Debian 上直接装不上

`sh` 走 `--version` 探针，而 Debian/Ubuntu 的 `sh` 是 dash：

```text
$ sh --version
sh: 0: Illegal option --
exit=2
```

没版本号 → `parse_version` 返回 `None` → `probe` 返回"cannot determine the version"（`interpreter.rs:400-407`）→ 安装被拒。同一份 manifest 在 bash-as-sh 的机器上又能过，于是表现为"某些机器能装、某些不能"。**`sh` 的 `version` 永远省略。**

### 6.3 忘了 `"ready": "exec"`

`exec.ready` 缺省是 `notify`（`plugin.rs:631-635`），daemon 会等脚本发 MCP 握手；脚本不会发，于是一路等到 start 超时才失败。症状是 `svc status` 长时间停在 starting，日志里没有任何脚本输出。

脚本插件一律 `"ready": "exec"` + 不声明 `capabilities`（`package.rs:572-580` 会拒绝这个组合）。反过来，真要 `ready=notify`，就得像 §4.5 那样把 `-u` 带上，否则协议帧被块缓冲吞掉。

### 6.4 把 `-ExecutionPolicy` 写在 `-File` 后面

`-File` 之后的一切都是脚本参数。本机实证：

```text
powershell -NoProfile -File echo.ps1 -ExecutionPolicy Bypass
  -> Action=[DEFAULT] argsCount=2 args=[-ExecutionPolicy|Bypass]
```

`-ExecutionPolicy Bypass` 安静地变成了脚本的 `$args`，**执行策略没生效**，脚本还照跑——直到遇到被策略拦下的环境（Server 默认 `Restricted`、UNC 路径、带 Zone.Identifier 的文件）才炸。同理，`-Command` 也不该用：`-Command` 后的整串被当代码解释，脚本路径带空格时行为不可控，它同样"必须是最后一个参数"。

### 6.5 `cmd /C` + 路径里的 `&` 或 `^`

框架构造的是 `cmd.exe /C "<绝对脚本路径>" <args>`。cmd 只有在满足一组条件时才保留引号；路径里出现 `& < > ( ) @ ^ |` 之一就会退回"剥掉首尾引号"的旧行为（[cmd](https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/cmd)：*"You don't use any special characters within the quotation marks, for example: & < > ( ) @ ^ |"*），引号一剥，路径在空格处断掉。

本机实测（同样的命令行，只改目录名）：

```text
.../spk interp test/payload/clock.bat        -> exit=0  BAT-RAN arg=start   （空格 OK）
.../spk (x86) test/payload/clock.bat         -> exit=0  BAT-RAN arg=start   （括号 OK）
.../spk & ampersand/payload/clock.bat        -> exit=1  'C:\...\spk' is not recognized…
.../spk ^ caret/payload/clock.bat            -> exit=1  'C:\...\spk' is not recognized…
```

默认安装根 `%ProgramData%\svcd\…` 不含这些字符，所以常规部署安全；但 `plugins_dir` 是可配的（`$PM/src/paths.rs:31-36`），配到含 `&`/`^` 的目录就会踩。另外 `%` 会被 cmd 做变量展开，属于同一族的隐患。

一句校准：文档把 `(` `)` 也列进"特殊字符"，但本机这一版实测括号**没有**触发退化。所以把 `&`、`^`（以及同族的 `|`、`<`、`>`）当**确定风险**，括号当**未定风险**——这类行为取决于 cmd 版本，别在模板里赌。

### 6.6 把解释器开关写进 `exec.args`

`exec.args` 拼在**脚本路径之后**（`slot.rs:934-939`），所以 `exec.args: ["-u"]` 会变成脚本的 `sys.argv[1]`，而不是 Python 的开关。所有解释器开关只能放 `interpreter.args`。同理 `lifecycle.*.args` 是 action 参数，不是解释器参数——同一个解释器被 `exec` 和所有 lifecycle 共用，没有"每个钩子单独配解释器"这回事。

## 7. 来源

### 一手（本仓库/本地实现，最权威）

| 内容 | 位置 |
|---|---|
| 解释器解析、PATHEXT 搜索、`.exe`/`.com` 过滤、探针分派、版本解析、约束语法 | `D:\learn\harmess_workspace\plugin-manager\src\interpreter.rs:41-44`、`:104-109`、`:121-166`、`:169-204`、`:246-277`、`:293-320`、`:341-349`、`:378-388`、`:400-409` |
| 运行时命令行拼装（`interp + args + script + args`） | `D:\learn\harmess_workspace\plugin-manager\src\slot.rs:900-942` |
| `exec.path` 解析为绝对路径、`ready` 缺省值、exec/lifecycle 共用解释器 | `D:\learn\harmess_workspace\plugin-manager\src\plugin.rs:612-743`、`:745-794` |
| 脚本 payload 与 `interpreter` 的互斥校验、`check_path`、`ready` 与 `capabilities` 冲突、schema 2 门槛 | `D:\learn\harmess_workspace\plugin-manager\src\package.rs:511-544`、`:551-556`、`:560-580`、`:625-650`、`:866-876` |
| `platform` 与 `TARGET` 精确相等 | `D:\learn\harmess_workspace\plugin-manager\src\package.rs:25-27`、`:318-324`；`D:\learn\harmess_workspace\plugin-manager\build.rs:1-4` |
| 解释器执行时放宽 exec 位 | `D:\learn\harmess_workspace\plugin-manager\src\bundle.rs:589-601` |
| `interpreter` / `lifecycle` 字段定义与语义、嵌入式预算、hooks 默认拒绝 | `D:\learn\harmess_workspace\plugin-manager\docs\specs\2026-11-12-核心层需求最终版.md:112-134`、`:151-166`、`:231-239`、`:258` |
| 跨平台脚样本（`%~1` 收 action、`exit /b`） | `D:\learn\harmess_workspace\plugin-manager\samples\clock\clock.bat:1-15`、`D:\learn\harmess_workspace\plugin-manager\samples\services.d\clock.bat:1-12`、`D:\learn\harmess_workspace\plugin-manager\samples\services.d\clock.sh:9-26` |
| 真实 python 解释器 manifest | `D:\learn\harmess_workspace\plugin-manager\samples\spk\wireguard-1.0.0-x86_64\manifest.json:1`、`D:\learn\harmess_workspace\plugin-manager\samples\wireguard\manifest.json:60-63` |
| 打包命令里的 `--interpreter cmd --interpreter-arg /C` | `D:\learn\harmess_workspace\plugin-manager\samples\services.d\README.md:66-81` |

### 外部一手（平台行为）

- [CreateProcessW function (processthreadsapi.h)](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-createprocessw) — 批处理必须经 `cmd.exe /c`；无扩展名时只补 `.exe`（因此 `.cmd`/`.bat` 不可能是 CreateProcess 的直接目标）。
- [cmd](https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/cmd) — `/C` 的引号保留条件与"特殊字符导致退回去引号"的规则；`&`/`|`/`()` 需转义。
- [about_PowerShell_exe](https://learn.microsoft.com/en-us/powershell/module/microsoft.powershell.core/about/about_powershell_exe) — `-File` 必须是最后一个参数，其后一切归脚本；`-ExecutionPolicy` 只改当前会话策略。
- [about_pwsh](https://learn.microsoft.com/en-us/powershell/module/microsoft.powershell.core/about/about_pwsh) — `pwsh -File` 同样的"必须最后"约束；`-ExecutionPolicy` 在非 Windows 上被忽略；7.2 起 Windows 上 `-File` 只接受 `.ps1`。
- [Using Python on Windows](https://docs.python.org/3/using/windows.html) — Windows 不预装 Python；推荐 `python`，多版本用 `py`；`python3` 只是"接住 POSIX 习惯"的别名，不推荐使用。
- [Node.js Command-line API](https://nodejs.org/api/cli.html) — `node [options] [<program-entry-point>] [--] [arguments]` 的签名，以及 `-v, --version`。

### 本机实证（2026-11-14，Windows 11 26200 / 26100，PowerShell 5.1 与 7.6.7 并存）

所有 `exit=` / 输出都是本机跑出来的，不是推断：`python3 --version` → `9009`；`python` / `py` → `Python 3.10.2`；`node` → `v20.11.1`；`cmd /C ver` → `10.0.26200.9168`；`powershell -Command '$PSVersionTable.PSVersion.ToString()'` → `5.1.26100.9168`；`pwsh --version` → `PowerShell 7.6.7`。`cmd /C` 的路径边界（空格 / 括号 / `&` / `^`）与 `-File` 之后参数去向，均见 §6.4、§6.5 的原始输出。Debian(dash) 的 `sh --version` 失败在 WSL Debian 上实测。
