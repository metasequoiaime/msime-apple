# Android 输入宿主

## 目录结构与验证入口

Java/Kotlin 宿主按 `java/app/msime/client/<feature>/` 分为 `account`、`candidate`、`clipboard`、`core`、`dictionary`、`handwriting`、`keyboard`、`policy` 和 `voice`；JNI/C++ 适配位于 `native/`，资源位于 `res/`，按职责组织的回归位于 `tests/<feature>/`，设备脚本位于 `tests/device/`。Tauri/React 设置仍复用 `packages/ui` 和 `apps/desktop/src-tauri/src/platform/android/`，不会在 Android 复制一套页面或 Rust 业务。

验证分三层，各有对应入口：`check-host.sh` 做契约守卫与 JVM 冒烟（CI 的 `ci-platforms.yml` android job 跑的就是这条），`build-native.sh` + `verify-native.sh` 做 arm64-v8a 与 x86_64 双 ABI 的原生构建与导出校验（包括在线候选的五个 host 导出与五个 JNI 方法），`tests/device/smoke.sh` 在固定的 API 35 arm64 专用 AVD 上跑 instrumentation，覆盖原生输入、Tauri/IME 合包、共享设置、统计与手写流程。

本目录的正式 Android applicationId 是 `app.msime.android`，原生类所在的 namespace 是 `app.msime.client`；两者不同但都属于同一个 Android 宿主。设备 smoke 使用独立的 `app.msime.client.test` instrumentation APK。`app.msime.client.preview` 是改名前的旧包名，本宿主不使用它，也不要为它新增入口或兼容分支。

### 手机、大屏与二合一布局边界

原生输入面依据 Android 的 `Configuration.smallestScreenWidthDp` 区分设备形态，而不是依据旋转后的当前窗口宽度。小于 600 dp 的手机始终让键盘铺满可用窗口；因此一台 411 dp 的手机即使横屏后当前宽度超过 600 dp，也不会突然切换成平板键盘。600 dp 起的平板、展开态折叠屏和二合一进入大屏布局：整套键盘表面在窗口底部水平居中，宽度取当前可用宽度与 720 dp 的较小值。左右空出的区域使用当前键盘皮肤的背景色，避免十列按键被拉伸到桌面宽度。

“整套键盘表面”包括候选区、字母/符号/九键/手写主键区、展开候选、剪贴板、输入方案、皮肤、布局调整、语音结果、AI 润色、更多工具、表情和符号面板；这些层必须共用同一 720 dp 外框，不能只限制字母键而让覆盖面板重新铺满屏幕。系统发生旋转、折叠展开、自由窗口缩放或外接显示器配置变化时，`MSIMEInputService.onConfigurationChanged` 只重新计算外框，再重算按键高度和间距；不会重建 Engine session，也不会清空当前组合、候选代次或面板状态。

这条原生键盘策略与共享设置页的响应式布局彼此独立：`platforms/android` 负责系统 IME 窗口，Tauri/React 设置页仍按自身 600 px CSS 断点在手机底部标签栏与大屏侧栏之间切换。`KeyboardFormFactorPolicySmoke` 固定验证 599/600 dp 边界、手机横屏、600 dp 折叠展开态、1280 dp 二合一的 720 dp 上限，以及配置暂时缺失当前宽度时的安全回退；`check-host.sh` 会编译并执行该回归。设备上的旋转、分屏、自由窗口和折叠铰链切换走的是同一条策略，JVM 回归钉住的是策略边界本身。

`check-host.sh` 在装有固定 NDK 28.2.13676358 的机器上额外用 `aarch64-linux-android28-clang++` 以 `-Wall -Werror` 对 `native/client_jni.cpp` 做目标平台编译：Java 里声明 `native` 的方法在没有 C++ 实现时照样能编过，而这是 Java 声明与共享 FFI 签名唯一必须一致的地方；完整原生构建需要 vcpkg 和 Engine，这一步不需要。没有固定 NDK 的机器会跳过并明确说明。`verify-native.sh` 的导出清单同时覆盖 online query、云 URL、AI 请求描述符和两个在线候选写回入口。

宿主 Java 以 API 35 的 `android.jar` 编译，而 manifest 声明 minSdk 28，因此比真实 APK 构建宽松；`Files.readString`/`writeString` 属于 API 34，本宿主不使用，`check-host.sh` 对这两个方法有定向检查，其余 API 级别问题仍由 Gradle lint 覆盖。`scripts/verify-local.sh` 另有 `compile: android target` 阶段，在固定 NDK、Rust `aarch64-linux-android` 目标与 vcpkg 依赖前缀齐备时检查 `msime-desktop` 的 Android 分支；宿主的 `cargo check --workspace` 只覆盖宿主目标。

`NativeClient` 提供 Java/Kotlin 到共享运行时的 JNI 传输。UTF-8 字节数组保留非 BMP 字符，避免 JNI modified UTF-8 损坏候选或资源路径。JNI 负责释放 C API 响应；上层解析 ok/value，负责会话线程和生命周期。

`MSIMEInputService` 提供实际 InputMethodService 源码、系统 manifest 和输入法元数据；最小 Android 28，编译目标 35。软键盘、硬件 ASCII 键、候选点击和翻页调用同一 JNI；Engine 提交与剩余编辑串通过 `EditorBridge` 按顺序映射到 InputConnection。宿主不实现输入算法或分页规则。密码、非文本和无建议字段直接输入，不创建 Engine；IME_FLAG_NO_PERSONALIZED_LEARNING 关闭当前会话学习。宿主不记录输入；网络权限只供用户明确启用并确认发送的 AI 请求使用。

外接硬件键盘的退格、左右方向、Home、End 和 Forward Delete 通过 `HardwareKeyPolicy` 映射到共享 Engine 的 0/4/5/6/7/8 命令；组字或候选状态由 Engine 处理，空闲时返回给编辑器。Ctrl/Alt/Meta 组合键仍交给系统快捷键，不把宿主命令抢走。这样 Android 的物理键盘不会复制一套编辑状态机，也不会把前删错误地当成普通退格。

共享 `number_row_selection` 开启时，硬件键盘数字行 1–9 选择当前候选页对应槽位；选择仍携带 Engine 返回的 session、generation 和候选 index，候选过期或当前没有该槽位时按键交回编辑器。英文、密码和直接输入不抢数字键，关闭偏好也立即恢复系统行为。

硬件键盘快捷键消费共享 `keybindings`：Shift+Space、Ctrl+Alt+Space 和单击 Shift/Ctrl 可切换中英，Ctrl+Shift+F 切换简繁，Alt+Shift+H 切换全角输入；每个开关都按偏好即时生效。修饰键单击只有在 600 ms 内且期间没有按下其他键时才触发，组合键优先于普通编辑器快捷键；不匹配或关闭的快捷键继续交给 Android/编辑器。触屏键盘的 Shift 和“简/繁”按钮仍走各自原生路径。

键值与键面是两件事：`KeyboardLayout.rows(layer)` 只给键值，字母恒为小写，因为这是交给 Engine 的形式，Engine 只能用小写字母起拼音组合；键面由 `LetterKeyFacePolicy` 单独决定，中文 26 键按 Apple 一律画大写。两者曾被合并处理，导致中文态把 `N` 发给 Engine、被拒后当字面上屏，26 键中文输入整体失效。

软键盘的主按键区按 Apple 键盘的基础层次拆成字母层和符号层；字母层支持可见的 Shift 状态，符号层保留标点、括号和数字，两个层次均通过无障碍描述暴露当前按键。层次排列由无 Android 依赖的 `KeyboardLayout` 提供，便于在主机测试中验证布局不被宿主生命周期改变。

“符”入口按 Apple 的整屏符号面板适配为 Android 原生面板：常用、中文、英文、数字、网络五类使用左侧分类和右侧五列滚动网格，底部提供返回、删除和锁定连续输入。打开前先由 Engine 完成组合；符号通过普通 `InputConnection` 以本地输入来源上屏，未锁定时插入一个后回到键盘，锁定时可连续输入。分类、网格数量和锁定行为由无 Android 依赖的 `SymbolPanelModel` 验证，宿主只负责 View 与触摸反馈。

编辑器上下文按固定 Apple 来源的边界适配 Android `inputType`：URI、邮箱、密码和明确禁用建议的字段临时进入英文输入，允许 Engine 的字段使用 dedicated English 模式，敏感字段继续绕过 Engine；离开后恢复进入前的中英状态，同一字段内用户通过“中/英”手动切换后，输入重启回调不会再次覆盖。英文模式始终展示完整 26 键，即使底层方案为九键或手写；数字和标点会在完成英文组合后由宿主直接提交，空格会完成候选并保留实际空格。`TYPE_TEXT_FLAG_CAP_CHARACTERS`、`CAP_WORDS` 和 `CAP_SENTENCES` 分别映射为全大写、单词首字母和句首自动大写，URI/邮箱强制关闭；规则只读取最多 128 个光标前字符并在内存中即时判断，不记录或持久化编辑器内容。缺失上下文安全回退为关闭自动 Shift。

输入模式的默认值和记忆范围也消费共享偏好：`default_ime_mode` 决定没有历史记录时进入中文还是英文，`ime_mode_scope=app` 时按 `EditorInfo.packageName` 记住用户手动切换，`global` 时所有编辑器共享同一个手动选择。包名只作为受限键名保存，不保存编辑器文本；URI、邮箱等字段触发的临时英文覆盖不会写入记忆，离开字段后恢复切换前的模式。包名缺失或格式异常时退回默认模式。

英文大小写状态继续对齐 Apple：中文态空组合点按 Shift 会先完成组合并进入英文的单次大写；单次 Shift 输入一个字母后自动回到小写，350 ms 内连续点按两次进入 Caps Lock，再次点按关闭。编辑器自动 Shift 与手动单次 Shift 使用同一三态状态机，但不会覆盖 Caps Lock；按钮以 `⇧` / `⇪`、选中态和“关闭 / 下一字母 / 自动开启 / 开启”的无障碍状态区分。切换符号层保留当前大小写，硬件 Shift 和本地模式触发使用每次事件自己的修饰状态，不污染软键盘状态；英文模式禁用中文本地输入工具。

中文全拼或双拼已有组合时，软键盘 Shift 保持中文会话并把后续字母以大写辅码交给 Engine，用于缩小候选；组合开始前仍按 Apple 行为切换到英文。五笔、日语、本地输入模式和空组合不启用辅码，Shift 的一次性状态在辅码输入后复位。

移动端智能标点消费共享 `smart_punctuation`、`chinese_punctuation` 和 `punctuation_lock`：中文跟随模式且 Engine 空闲时，逗号、句点或冒号紧跟 ASCII 字母/数字会保留 ASCII，锁定中文或英文优先；已有组合、日语、英文和本地模式仍交给 Engine。Android 每次只从 `InputConnection` 读取光标前最多两个 UTF-16 单元并向共享策略传一个 Unicode 标量，不保存或记录编辑器文字；缺失或异常上下文安全回退到 Engine 标点。

重复标点和标点后空格也由共享 Host API 决定：Android 只在当前编辑器会话内保存带 `editor_generation` 的有界 snapshot，按下下一个标点或空格时重新读取光标前标量并消费 `replace_with` / `space_ascii`；焦点、会话或编辑器变化会清空 snapshot，过期或上下文不一致时不改写文本。重复时间窗口、候选数量、组字状态和开关均不在 Android 重实现。

微软双拼在字母第二行额外提供“微软双拼 ing”分词键，只有中文微软双拼普通输入时显示；它把 `;` 原样交给 Engine，由 Engine 根据当前组合决定 ing 韵母或标点语义。英文、日语、五笔和本地输入模式不显示该键。

双拼键位提示由 Engine 的 profile 表通过共享 Host API 提供，Android 不维护第二份键盘映射。提示中的 ` / ` 分隔声母侧与韵母侧，同一侧的多个单位以空格分隔；因此一个键可能同时显示多个韵母（例如小鹤 `K` 的 `ing uai`）。切换双拼方案后按 profile 刷新缓存；未知方案、损坏响应或原生失败直接隐藏提示，不用其他方案的标签误标当前键盘。提示只在中文双拼、非本地模式且非 dedicated English 时显示。

顶部“简 / 繁”快捷键消费共享 `traditional_chinese_output` 偏好，只在 Android 展示与插入边界用共享 OpenCC s2t 转换（`msime_client_simplified_to_traditional`，与 Windows、macOS、iOS、HarmonyOS 同一套词表）：Engine 候选原文、候选身份、组合文本和输入算法保持不变。候选条、展开候选面板、Engine 最终提交和手写候选使用同一规则；日语方案、临时日语和 dedicated English 保留原文。快捷键通过共享 revision CAS 乐观刷新当前候选，冲突或写入失败恢复最近接受值；顶部语音入口开启时让出同一快捷位，高情商回复优先于语音。转换器拒收的文本保留原文，不伪装已转换。

“全角输入”沿用 Apple 键盘扩展的直接输出边界：Android 更多工具页提供持久化开关，宿主明确直写的 ASCII 字符、空格和九键字面在开启后转换为 Unicode 全角；Engine 的中文组合、候选身份、手写结果、日语和本地模式保持原文。专用英文模式虽由 Android Engine 管理组合，但其最终英文 commit 在同一宿主边界转换，保证键盘内英文候选与 Apple 的直接英文输入一致。

引擎不接受的标点按 Apple `handleSymbol` 的边界处理：组字中时先用共享宿主命令 9（`Action::Finish`）按首选候选结束组合，再由宿主把该标点上屏。Android 的预编辑是真正的 composing region，直接 `commitText` 会替换掉正在组的拼音，于是「nihao」后按 `@` 只剩 `@`；现在得到「你好@」，与 Apple 和 macOS 的 finish_composition 一致。没有组合时照旧直接上屏，被拒绝的数字仍是当前页没有对应候选的候选键，不走这条自动上屏。边界由无 Android 依赖的 `DeclinedKeyPolicy` 提供。

收起键盘或输入视图结束时，若仍在组字，按首选候选结束组合后上屏，与 macOS 失焦边界的 `MSIME_FINISH_COMPOSITION` 一致；编辑器因此留下 `你好` 而不是字面 `nihao`。

回车键按当前 Android `EditorInfo` 显示并执行前往、搜索、发送、下一项、完成或上一项动作；无明确动作、未知动作或编辑器设置 `IME_FLAG_NO_ENTER_ACTION` 时显示“换行”并提交换行符。执行前先通过共享 Engine 完成当前组合；若组合已被处理，回车到此为止，不再误触发编辑器动作或追加换行。日语九键侧栏同步显示“改行/確定”，底部全局回车仍保留 Android 的 `EditorInfo` 标签和 dispatch 适配，条件由同一个纯 Java 契约提供。

工具栏“空格”支持轻点选词或插入空格，也可左右滑动向编辑器发送有界方向键事件以移动光标。滑动开始时先完成 Engine 组合，距离累积器绑定当前 `InputConnection` 身份；输入目标变化、手势取消、非有限坐标或异常跳变都会终止移动，不读取或持久化编辑器文本。

无障碍增减键盘高度每一步都会保存：拖动在松手时 commit，而无障碍调整没有松手这一刻，只预览会被下一次偏好应用覆盖回原值。

键盘工具栏的“设置”面板以远端默认分支固定来源 `MSIME-Apple@3d300cdc62fe0d09565b30bd3e4165571fb91562` 复刻透明实时调整层：键盘保持可见，键盘区域左右拖动按 Apple 的主轴锁定规则调整按键间距，上下拖动调整行间距，顶部工具条拖动把手调整高度；Android 额外保留顶部语音入口开关。高度在平台默认键区基础上支持 -12–+48 dp，并以整数写入共享 `touch_keyboard_height_adjustment`；26 键三行均分增量，九键整体增减，手写把增量用于书写与工具区。间距支持 3.0–6.0 dp 和 4.0–10.0 dp，并以 0.1 dp 精度写入共享 `touch_key_spacing_tenths` / `touch_row_spacing_tenths`。拖动时直接更新已有 View 的高度或 margin，不重建按键树、Engine 或丢失当前组词与手写笔迹；松手、无障碍增减及切换语音入口后通过共享 revision CAS 保存，冲突或写入失败会恢复最近一次已接受快照。

剪贴板历史的拒绝理由按 Apple `ClipboardHistoryStore.Failure` 分开命名：空白文本、单条超过 10,000 字或 40,000 字节、以及 50 条全部固定各有自己的提示，最后一条明确要求先取消固定或删除一条；Android 没有 iOS 的粘贴授权提示，空白文案相应去掉该从句。全部固定是独立的 `ClipboardHistory.FullException`，与读不出或写不回历史文件的普通 `IllegalStateException` 分开，避免把用户指向错误的动作。面板状态行同时说明点按插入以及在「管理」中固定或删除。理由分类与文案由无 Android 依赖的 `ClipboardHistoryPolicy` 提供并在 JVM 回归中验证。

语音结果按 Android 平台能力适配：独立 Activity 调起用户设备上的系统语音识别服务，录音由该服务持有，MSIME 只接收有界文本。主应用进程与独立 `:ime` 进程通过应用私有目录中的非阻塞文件锁交接最新一条结果；结果最多 10,000 个 Unicode 码点、10 分钟有效，并在插入前一次性 claim，避免两个键盘实例重复插入。键盘“更多”工具页提供与 Apple 同级的语音结果入口，结果面板内提供 Android 平台的系统语音识别入口；共享 `touch_voice_shortcut` 开启后，候选栏显示直达语音结果按钮。存在 Engine 组合或本地模式时拒绝打开结果，确认插入前还会比对 InputConnection 身份、选择位置 generation 及光标前后/选中文本快照；真实上下文仅短暂保存在内存，不写日志或交接文件。

本地语音识别（共享设置里 provider 为 `local`、`asr_model_path` 指向共享安装器写好的模型目录）在同一个语音 Activity 里用 sherpa-onnx 在本机识别，音频不离开设备：共享层只在路径为绝对路径时下发 `modelPath`，宿主再确认目录里有 `msime-model.json` 才开始。识别复用 `shared/voice/LocalAsr` 与桌面同一套清单、热词和 VAD 逻辑，经 JNI 编进 `libmsime_android.so`；运行时 `libsherpa-onnx-c-api.so` 与 `libonnxruntime.so` 由 `build-native.sh` 通过 `scripts/fetch_voice_runtime.py` 按 `resources/voice-runtime.lock.json` 下载校验后只从 .aar 中取出，不进版本库，并与其它原生库同样检查 16KB 对齐和依赖白名单。录音、模型加载与解码都在后台线程，边说边把部分结果显示在录音窗口；热词来自用户词库，经共享 `msime_client_voice_hotwords` 读取，清单声明 `pinyin` 模式的模型在识别后再经 `msime_client_voice_hotword_correct` 按拼音纠正。模型闲置 2 分钟后释放，系统回收内存时立即释放。模型缺失或损坏、运行时无法加载时直接提示错误，不会退回系统语音识别服务。

AI 润色对齐固定 Apple 来源的确认式流程：仅在 Engine 空闲且编辑器存在非空选区时显示入口，输入和输出各限制 10,000 个 Unicode 码点。全屏面板明确展示 HTTPS 目标 origin、模型和待发送文字，用户再次点按后才发起 Chat Completions 请求；单线程请求队列容量为 1，关闭面板或点击取消会中断任务并断开连接，响应限制为 1 MiB。请求前、响应后及最终替换前均校验 InputConnection、选区 generation、光标前后文本和完整 AI 配置；过期结果不会展示，结果也绝不自动插入。操作按钮固定在面板底部，长文本不遮挡取消或替换。共享设置按规范化的 endpoint origin（HTTPS 主机和端口）保存 Token，同主机不同路径可复用，主机或端口变化时不会沿用；日志、测试和诊断不包含选区、结果、Token 或原始响应。Android Tauri 设置页额外提供由原生 HTTPS transport 执行的模型目录读取、可用模型选择和确认式润色测试；模型分页、服务能力筛选、凭据和 1 MiB 响应均有边界保护。

“试用键盘”页也提供 Apple 同级的 AI 对话入口：用户登录后显式加载 `/v1/models`，从有界模型菜单选择模型，消息按最多 14 条、每条 10,000 字符和总上下文 48,000 UTF-8 字节裁剪；发送前不会读取或上传输入框之外的内容。请求在后台执行，支持停止、失败提示和过期结果丢弃，响应只保留在当前 Activity 内存中；未登录时不伪造可用的模型下拉。

表情面板和手写板各自消费共享的 `emoji_theme` 与 `handwriting_theme`：表面显式 `dark`/`light` 覆盖全局 `theme`，`follow` 继承全局，全局为 `system` 时跟随 Android 夜间模式；缺失或无法识别的值按 `follow` 处理，不会让这两块面板在旧快照下单独翻到浅色。两者使用与键盘同一套皮肤标识和自定义设计，只是明暗解析不同；键盘整体着色照旧走 `screen_keyboard_theme`，候选栏继续由 `candidate_theme` 决定，偏好热更新只重画表面，不重建 Engine 会话或手写笔迹。`menu_theme` 没有 Android 消费者——系统 PopupMenu 由平台绘制——共享设置因此不再在移动端显示该项。

云联想与 AI 联想按 Windows/macOS/Linux/HarmonyOS 已有的共享 provider 边界接入 Android：组字停下 350 ms 后，宿主向共享 host 索取 `online_query`，再由单线程 worker 分别执行云候选 HTTPS GET 和 AI Chat Completions POST；URL 与请求描述符都由共享 host 构建，凭据留在 session 内，宿主只搬运字节。两者都是可选偏好：`cloud_candidates` 关闭即不发起云请求，AI 联想还要求 AI 辅助已启用且配置完整。**开启云联想意味着把当前正在组的拼音发送给云输入服务**，与其他桌面/移动宿主的既有行为一致，可在共享设置中关闭。请求身份由 session、cache key、identity、云开关和启用状态下的 AI 配置组成，同一组合只问一次；epoch 保证上一段组合的迟到结果不会写入新会话。云结果会推进 Engine 代次，所以 AI 请求在云结果落地后重新读取 query 再发出。云响应上限 256 KiB、AI 响应上限 1 MiB、AI JSON 内容上限 64 KiB，单条候选上限 4096 字节，空白、含控制字符、重复和超限候选被跳过而不影响同批其他候选；候选数量上限取共享配置。这些边界由无 Android 依赖的 `OnlineCandidatePolicy` 提供并在 JVM 回归中验证。

打字统计按固定 Apple 来源只记录成功上屏的 Unicode 扩展字符簇，空格、换行和未上屏按键不计，组合表情计为一个字符。提交内容只在 Rust 内存中分类，持久化文件只含日期、字符类别、提交来源和数量，不保存输入原文。分类覆盖汉字、拉丁字母、其他文字、数字、标点、表情、其他符号与旧版未分类；来源覆盖全拼 26/9 键、四种双拼、五笔、日语、手写、英文、本地输入、AI 润色、高情商回复和语音。每日计数与分类默认永久保留，按保留策略清理的日期同时从累计总数与分类中扣除；启停与清空使用同一跨进程文件锁，清空不会重新启用统计。写入通过容量 32 的单线程队列离开输入主线程，队列满或存储失败不保留待写文字，且每个输入会话只显示一次脱敏错误提示。

Android Tauri 设置仅在 Android WebView 注入统计能力，桌面设置不显示入口。页面提供 7 天、30 天和累计范围、最近 7/30 日趋势与单日下钻、字符类型/语言模式/输入方案占比、刷新、即时启停和确认清空；统计页独立于 Preferences 草稿和“保存设置”。主应用与独立 `:ime` 进程共享 `files/bootstrap/state/typing-statistics.json`，由锁文件串行读写；页面会区分从未写入和已清空状态，并明确说明本机只保存聚合计数。完整 arm64 Tauri 合包已在专用 API 35 arm64 AVD 验证实际上屏聚合、文件不含合成输入文本、页面跨进程读取、禁用后不增长、取消/确认清空、清空保留禁用状态和重新启用后累计。

“高情商回复”是独立 Android 宿主方案，底层固定映射到 Engine 的全拼 26 键，不向共享输入算法增加 AI 状态。选中后在共享候选/快捷栏下方显示占据剩余键区的专用面板；“帮你回/帮润色”和社区模板位于面板内，输入方案、共享触屏键盘皮肤和收起入口继续复用上方共享快捷栏，避免重复入口；正文通过用户明确点按读取当前文本剪贴板，限制 10,000 个 Unicode 码点，并提供九种内置风格、删除、清空、取消、生成和同风格“换一句”。回复请求复用 AI 润色的 HTTPS transport、容量 1 队列、取消和 1 MiB 响应边界，但每次使用 Apple 对应风格 prompt；候选去重、最新在前且最多三条，服务结果绝不自动上屏，只有点按候选且 InputConnection、光标上下文、Engine 空闲状态、当前方案和完整 AI 配置仍匹配时才插入。插入后面板让出编辑器，候选栏“回复”入口可重新打开。切换模式、修改源文字、切换方案、编辑器上下文或 AI 配置变化都会取消请求并废弃迟到结果。

社区回复模板只从应用私有 files 目录的 `CommunityLibrary.json` 读取，该文件用于主应用向独立 `:ime` 进程显式共享已收藏资源，不包含凭据或源消息。读取拒绝符号链接、非 UTF-8/非法 JSON、超过 4,000,000 字节、超过 50 项或无效字段，仅展示 `kind == reply` 且含 prompt 的条目；发起请求时重新读取，已移除模板不会复用。高情商回复与其他触屏输入方案统一由共享 `touch_keyboard_schemes` 管理可见性和选择；只有旧快照尚无该字段时，原 IME 私有开关与选择才作为迁移兼容来源。

候选区独立显示当前组合文本、当前页和候选按钮；Engine 候选超过当前页容量时显示展开入口。用户打开面板后，宿主通过按需 host API 一次复制当前 generation 的完整 Engine 候选，在顶部显示 preedit、候选总数和收起入口，候选 chip 按实际测量宽度自动换行且不绘制序号；全局序号只保留在无障碍描述中。展开面板没有分页按钮，点按页外候选通过独立的全代次选择 API 交回 Engine。普通候选栏继续使用分页 `View` 和当前页选择边界，每次按键不会携带完整列表；两条选择路径都校验 session、generation 和全局索引，宿主不复制候选算法或组合状态。

候选英文释义按固定 Apple 来源 `MSIME-Apple@d117009573a1a619cfb1702645f38c3b4c378a78` 实现，并通过共享 `candidate_english_gloss` 偏好选择性开启，默认关闭。开启后，Android 只在 IME 主线程复制当前 generation 的完整候选；无 session 的有界 worker 请求由 C++ Engine bridge 只读访问随包 `english.db`，Java 不实现输入算法也不读取 SQLite。完成结果返回主线程后必须同时匹配 session、generation 和生命周期 epoch，才会调用共享 `apply_translations`；停止输入、替换会话、偏好变化与服务销毁都会使旧结果失效。Engine 的五笔等候选提示优先占用次要文本位置，离线释义仅在没有 Engine 提示时显示；候选条与展开面板以较小的皮肤兼容文字和不同无障碍说明展示，候选身份、点击选择和上屏原文不变。查询失败静默保留普通候选，不记录候选文字；关闭偏好会立即隐藏已返回释义，缺失资源不会创建数据库或用户数据。

非英文目标语言（fr、ja、es、ru、de、ko）的离线释义来自 `scripts/build_offline_glosses.py` 生成的 `zh-<lang>.db`。`build-apk.sh` 与 `build-client-apk.sh` 在 `target/offline-glosses`（或 `MSIME_OFFLINE_GLOSSES` 指定的目录）同时有数据库和 `offline-glosses-NOTICE.txt` 时把它们打进 `assets/offline-glosses/`，没有则照常构建。每次打开 MSIME 应用时 `Bootstrap.prepare` 都会检查（已有运行配置也一样）：安装包的 `lastUpdateTime` 变化时把它们解压到资源目录旁的 `files/bootstrap/offline-glosses/`，不含它们的新包会清掉旧文件；这一步不属于已校验的运行配置，失败只影响非英文释义。同一个 `candidate_english_gloss` 开关控制它们；已安装词典的目标语言按用户的目标顺序与英文释义、账户翻译逐候选合并，离线释义优先，账户翻译只补离线没有的行。日文方案与临时日文模式不请求其他语言释义，与账户路径一致。

触屏键盘皮肤以固定 Apple 来源 `MSIME-Apple@11c950a63ec57656cd78b3f75aa621c293bfe453` 为基线，按相同顺序提供水杉绿、海盐蓝、浅蔷薇、素白瓷、纸上时光、奶油桃桃、霓虹夜航和工程蓝图。共享 `touch_keyboard_skin` 与桌面候选窗的 `candidate_skin` 完全独立；React 屏幕键盘页、普通输入方案和高情商回复键盘消费同一个选择。Android 适配保留 Apple 的明暗调色、圆角、边框、阴影、等宽字体以及网点、网格和波纹背景，`screen_keyboard_theme` 优先于全局 `theme`，两者都跟随系统时读取 Android 夜间模式。键盘内选择通过共享 revision CAS 保存，失败恢复最近一次已接受皮肤；设置热更新只重新应用视觉样式，不重建 Engine 会话。未知 ID 安全回退到水杉绿，不把用户设置值当作颜色或资源名直接使用。

命名自定义皮肤图库沿用同一固定 Apple 来源，最多保存 12 套设计，名称去除首尾空白后限制为 32 个扩展字素。图库独立保存到共享状态目录的 `CustomSkins/library.json`，文件上限 9,000,000 字节；照片仍受单张 512,000 字节边界约束。Tauri command 每次在独立文件锁内读取最新文件，再原子执行新建、重命名、用当前设计更新或删除，避免多设置窗口以旧整表覆盖。图库操作不会增加 preferences revision，也不会进入输入法每次读取的热路径；新建设计会把当前页面草稿选择为 `custom`，应用、编辑和选择仍需通过页面底部“保存设置”写入普通共享偏好。损坏或超限文件不会被默认值覆盖。

“我的皮肤”继续使用同一固定 Apple 来源的 `CustomKeyboardSkin`、`SkinKeySurfaceView`、背景绘制和 `CustomSkinEditorView`。共享 `custom_touch_keyboard_skin` 保留 Apple 的 camelCase 字段和默认值，颜色限制为 24 位 RGB，圆角、边框、阴影、键帽透明度、纹理强度、照片压暗与位置均按 Apple 范围验证；照片只接受有界 base64 图像，解码后最多 512,000 字节。React 编辑器提供九种背景预设、14 套固定设计模板、渐变方向、照片缩放缩略图、三种纹理、文字对比度提示与优化、四种键帽造型、四种材质、撤销/重做和“使用皮肤”；当前设计随普通设置保存。Android 原生键盘不是只显示预览，而是实际绘制照片铺满与压暗、渐变、纹理、卵石/票券/胶囊/圆角轮廓和哑光/立体/玻璃/纸张键帽，并在同一 `custom` ID 的设计变化后原地重绘。Apple 的最多 12 套命名图库已使用上一段所述独立有界文件，避免把多张照片带入每次键盘读取的偏好；社区下载试用、评分、发布、我的作品范围和下架均已接入同一社区页，账号页可直达本机设计及这些社区集合。

“更多”入口现在使用与 Apple 同层级的全键盘工具页：顶部返回，剪贴板历史、AI 润色和语音结果为单列 48 dp 卡片，按键反馈为两列，轻/中/强振动为三列，本地输入为两列并可纵向滚动。选中、启用、禁用和不可用状态通过按钮状态与无障碍描述同步暴露，不再依赖锚定底栏的系统弹出菜单。按键、候选、翻页和面板操作共用反馈路径；按键反馈保存到主应用与 `:ime` 进程共同读取的 `files/bootstrap/state/keyboard-feedback.json`，以临时文件原子替换，旧版 `keyboard-feedback` 偏好只用于首次迁移和兼容回写；输入法每次显示时重新读取，默认按键音开启、振动关闭。振动使用 Android `VibrationEffect`，没有振动器时回退到系统键盘触觉反馈。

独立表情浏览器以远端默认分支固定来源 `MSIME-Apple@41c5db42184f505bb889f6efb88cf17d7e58901e` 为行为基线，在空闲候选栏和“更多”工具页提供入口。打开前先由 Engine 完成已有组合，面板提供返回、删除、固定 Unicode 顺序的笑脸／人物／动物／食物／旅行／活动／物品／符号／旗帜分类，以及每行八个的可滚动网格。Android 不复制 SQLite 读取器或内置 1,935 条表情，而是在后台通过共享 host API 以 64 行游标页读取已验证 `others.db`；异常响应、超限字段、停滞或倒退游标会被拒绝。选择通过本地输入来源写入普通 InputConnection，成功后将去重、最近优先且最多 24 项的历史保存到输入法私有偏好；删除也走正常宿主编辑路径。API 35 arm64 专用 AVD 已验证组合完成顺序、分类切换、跨页加载、插入、删除、返回、最近使用和 IME 重绑后的持久化。

“更多”工具页的“本地输入”分组接入共享 `local_modes` 偏好和 Engine 的 Shift 触发契约，按 Apple 顺序提供 Unicode、日期时间、超级简拼、快捷短语、英文补全、表情、颜文字和临时日语入口。禁用项或不支持本地工具的五笔/日语方案会置灰；宿主只发送触发字符，不实现本地模式算法。本地模式激活后，候选条显示独立的“退出本地模式”按钮，发送 Engine 的取消命令并保留共享输入状态边界。

键盘工具栏与 Android 设置按 Apple 固定顺序共享全拼 26 键、全拼 9 键、小鹤/自然码/微软/首道双拼、86 五笔、日语 9 键、日语 26 键、手写和高情商回复。`touch_keyboard_schemes.enabled` 控制快捷切换中可见的卡片并至少保留一种，`selected` 保存当前方案；隐藏当前方案时按固定顺序回退到第一种可见方案，设置与输入法进程重启后继续生效。键盘内切换先由 Engine 完成当前组合，再在后台通过共享 PreferencesStore 的 revision CAS 同步 `scheme`、`last_chinese_scheme`、`shuangpin_profile`、平台无关的 `touch_keyboard_layout` 及嵌套方案选择，保存成功后才更新当前会话；冲突或存储失败保留原方案。旧偏好默认全部可见和 26 键，并在第一次键盘内切换时迁移；`View.touch_keyboard_layout` 只报告已应用值，外部设置延迟时不会提前换布局。

全拼 9 键使用与 Apple 相同的分词/ABC–WXYZ 九宫格、常用中文标点，以及右侧的删除、句点和数字 0，并显示 Engine 返回的拼音消歧条。句点走与其他标点相同的 Engine 策略：中文标点开启时输入 `。`，英文或关闭中文标点时输入 `.`，数字键面中仍作为小数点使用。旧版占据右侧一个固定键位的“重输”已移除；现在长按退格时，若 Engine 正在组字，就一次取消整段组合并立即停止连删，若没有组合才持续删除编辑器正文。短按退格仍只删除一次。2–9 键支持长按弹出该键的数字和小写字母，选择字面字符前先由 Engine 提交当前组合，再通过普通编辑器路径上屏。数字和拼音选择都进入共享 Engine，拼音选择携带当前 generation，过期选择不会作用于新输入；数字语义严格跟随 Engine 的 `View.nine_key`。九键英文候选按固定来源 `MSIME-Apple@1a0d7194bba6ae1ee4555a7d2866bfb06a9fca6b` 和 `msime-engine@15ff08fc50ff9b469dae0f4bdabeae76c2a66b91` 接入，继续由共享 `mixed_input.english` 与 `minimum_prefix` 控制，Android 只发送数字、展示并选择 Engine 候选。合成加权词典回归验证完整编码 `65` 的 `ok` 排在更高频前缀词 `old` 前；候选顺序由 Engine 和当前锁定词库的权重决定，Android 只发送数字、展示并选择 Engine 返回的候选。

全拼 9 键按“符号”切到数字键面时仍然是九键，与 Apple 一致，不再交给 26 键符号页——那会把十列键盘塞进用户特意选的三列布局。九个网格键改印自身数字（含只喂分词符的 1 键），无障碍描述改为“数字 N”，点击以本地输入来源直接上屏数字而不进入拼音会话；标点列、删除、句点和 0 键保持不变，返回字母键面恢复分词/ABC–WXYZ 键面与长按弹出。两套九键都自带数字键面，因此快捷条的 Shift 在它们的数字键面同样隐藏。键面与描述由无 Android 依赖的 `NineKeyLayout` 提供并在 JVM 回归中验证。

日语 9 键复刻 Apple 的三列四行五向假名布局：轻点输入中间假名，按住并向左、上、右、下滑动时显示不拦截触摸的五方向预览，抬起后选择对应假名；や键提供「」，わ键提供わ/を/ん/ー/〜，第四行提供、。？！…。符号层仍保持三列网格，提供数字、括号和常用日文符号。“小゛゜”只在存在日语组合时启用，直接发送共享 host 命令 10，由 Engine 按当前最后一个假名循环小假名、浊音和半浊音；Android 不保留另一份假名变体表，也不把变体模拟成新输入。数字符号层的同一位置改为括号入口。普通假名仍只把对应罗马字逐字符发送给日语 Engine，不在宿主实现假名组合或转换；长音使用 Engine 接受的 `-` 罗马字。`check-host.sh` 同时锁定命名常量、FFI 的 `CycleKanaVariant` 映射以及宿主不得重新引入变体表。

手写提供 Android 原生画布和平台识别器注入边界：画布限制 64 笔、每笔 512 个采样点，支持单笔撤销、清空、坐标夹取和尺寸变化失效；识别请求复制不可变笔画快照，并以 session/revision/generation 拒绝过期结果，候选去重后最多 12 项。原生宿主与 Tauri 合包复用 `platforms/android` 中同一套 ML Kit Digital Ink Recognition 19.0.0 适配器和 `zh-Hani-CN` 模型，执行模型检查/下载、书写区域与时间戳笔画转换；专用 provider 在隔离的 `:ime` 进程中初始化 ML Kit 与其下载任务，设置主进程是否存活不影响手写。缺少适配器的源码级测试仍通过注入边界安全回退。与固定 Apple 来源 `MSIME-Apple@13cb320eb4ef662bbce0be3c73a5f34d68e86d80` 的平台取舍一致，Android 原生构建明确排除未调用的 Engine zinnia 识别器及其模型路径，桌面宿主继续保留共享 Engine 离线手写后备。手写方案、键盘内候选确认、无墨迹删除及退出清理已接入；有墨迹且候选已就绪时，软/硬件空格与回车确认第一候选，硬件退格和底部退格优先撤销最后一笔。专用 API 35 AVD 验证模型下载或既有模型就绪、真实触摸笔迹识别、第一候选确认、符号层往返和方案恢复。

中文候选在支持个人词典管理的方案中提供与固定 Apple 来源一致的长按菜单顺序：优先显示、固定到首位、取消固定和删除词条；删除操作要求 Android 确认对话框。固定位置通过共享 host API 限制为 1–5，本界面固定到首位时只传入位置 1。候选身份仍由 Engine 返回的 session/generation/index 传入 JNI，generation 或候选身份过期后不会修改当前会话；五笔、日语和本地输入模式不展示管理菜单。

候选 UI 现在消费共享的 `candidate_layout`（兼容旧的 `candidate_orientation`）、`candidate_font_size`、`candidate_preedit_font_size`、`candidate_font_family`、`candidate_english_font`、`candidate_fallback_fonts`、`candidate_skin`、`candidate_theme` 及候选颜色覆盖；Fluent、微信绿、石墨 Graphite 和杨柳青 Willow green 四套候选皮肤按明暗主题渲染到普通候选栏和展开面板，候选按钮、预编辑、页码和英文建议使用设置中的首选字体，缺字回落交给 Android 系统字体链。非法皮肤、主题、颜色和字体安全回退，显式候选文字颜色还按共享规则派生半透明编号色。偏好热更新成功后立即调整候选排列、字号、字体和调色板，不重建 Engine 会话。字号只接受核心偏好允许的 12–32 范围，字体名称遵循共享的 128 UTF-8 字节和控制字符边界。

横向候选条在 Engine session、generation 或候选页变化时回到当前页首项；这样翻页或新组字不会沿用上一页的横向偏移，把用户带到旧列表的中段。仅释义/翻译等同一代次的显示重绘保留用户当前滚动位置。候选身份仍由共享 session/generation/index 决定，Android 不自行排序或改写候选内容。

纵向候选布局也使用同一条滚动身份围栏：切换 session、generation 或 page 时同时回到纵向列表首项；同代次的候选附加信息重绘不打断用户位置。

候选词本身始终保持单行完整显示：候选按钮关闭 Android TextView 的自动省略与折行，横向候选条由外层滚动承载超长词，展开面板由候选布局负责换行候选格而不是在格内拆词。释义仍是候选的附加显示数据，不改变候选文字或选择索引。

候选 chip 的行数按当前候选实际渲染出的释义文本决定，而不是按全局请求的语言数量猜测：没有释义或只有一行释义时保持单行；只有 annotation 实际包含换行时才保留第二行。这样某个候选缺少次要语言释义时，候选词不会被错误折行。

普通触屏候选 chip 不把 1、2、3 等序号画进候选文字；序号只通过无障碍描述提供，并继续作为硬件数字行选择的槽位。这样长按、点击和数字行选择共享同一候选身份，同时保持 Apple 触屏候选的纯文字外观。

Tauri/React 共享设置页现在按 Android 原生能力展示候选字体、字号、颜色和边框/悬停控件；Android 不枚举桌面系统字体，字体框允许用户输入完整字体名，保存后由输入法进程消费。这样设置页不会把 Android 未实现的桌面字体目录能力伪装成可用功能。

“更多”工具页提供键盘内剪贴板历史面板。与 Apple 一致，只有用户点按“保存当前剪贴板”时才读取 Android 文本剪贴板，不后台监听；最多保存 50 条，支持去重、固定、删除、确认清空和点按插入。历史放在输入法私有偏好中，应用禁用备份且不记录内容；共享 `clipboard_history` 关闭时立即清空并禁用入口。非文本、空白或超过 10,000 UTF-16 单元/40,000 UTF-8 字节的内容不会保存。

Tauri 合包的 Android 账号页通过共享云剪贴板面板使用账号会话访问 HTTPS 云端接口；不自动读取系统剪贴板，只有用户明确在面板中添加文本时才上传。列表搜索、文本、64 位小写 hex ID、更新时间和分页均由共享 Rust/host 与 Android transport 做边界校验，单页最多 50 条。面板提供启用开关、搜索、添加、删除和复制操作，复制通过 Android `ClipboardManager` 写入系统剪贴板，不尝试桌面输入目标注入。独立原生 APK 保留同等能力的原生 fallback 页面。关闭云剪贴板会删除云端历史；账号会话、云端内容和系统剪贴板均不写入日志。

Android 账号设置中的云词典现通过同一认证会话访问 HTTPS API，内联提供四类词库的查询、分页、添加、编辑、删除、UTF-8 TSV/汉字自动注音导入和标准/Windows 导出；“完整目录”页按编码、全拼/双拼方案查询基础词库与账号覆盖，支持分页、编辑和显式删除；“云端候选排序”页按全拼/双拼/简拼查询候选，支持 canonical pinyin 调频、固定位置、取消固定和删除；云词典面板还提供快照导出、校验、恢复到云端，以及下载并在输入法空闲边界应用到本机的流程。请求、响应、64 位小写 hex ID、权重、格式、文本、编码方案、候选调频参数、固定位置和页长由共享 Rust/host 边界验证。导出文件只在用户点击导出时取得，不自动把云词库合并到本机。账号会话和云词库内容不写入日志。

个人词库设置同时提供 Apple 对齐的 `msime-personal-dictionary` JSON 导入：文件最多 1 MiB、每次 1–128 条，支持拼音、五笔、快捷短语和英文，页面先在本地校验格式、编码、重复项和长度并展示预览，用户确认后才加入 Android 与 `:ime` 进程共享的逐条同步队列。导入文件不上传；同步失败的条目沿用现有重试/移除记录机制，Engine 仍负责最终规范化和应用。

配置缺失、原生库不可用或输入连接错误会显示状态并退回直接输入。服务从应用私有 files 目录读取 `runtime-options.json`，路径必须指向已在设备上准备的词库与私有用户目录，不能复制 macOS 的配置路径。开发 APK 的启动页提供首次资源准备。

本地检查：`ANDROID_SDK_ROOT=<SDK绝对路径> bash platforms/android/check-host.sh`。需要 JDK 17+、Android API 35 和 build-tools 35.0.0，以及 `rg`（缺它会让脚本里所有 `if rg` 守卫静默通过，因此脚本直接拒绝运行）。脚本先逐条比对 Android 侧与 `crates/host-api/src/ffi/input.rs` 的共享命令契约，再用 `javac --release 17 -Xlint:all -Werror` 编译不依赖 Android 框架的那部分宿主 Java（排除 `java/app/msime/client/home/` 与任何 import AndroidX/Material 的文件——那些是 AAR，只有 Gradle 能解析），执行 72 个不依赖 Android 运行时的策略冒烟，并用 `aapt2 compile` 校验 manifest/resource；中间资源包随临时目录清理，不作为 APK 交付。

JVM 冒烟只覆盖无 Android 依赖的策略层；焦点、选区、编辑器动作和进程生命周期由下面「专用模拟器验收」一节的 instrumentation 覆盖。React 设置页的 Android 合包与验证见下文。

## 两个 APK 入口，不要选错

`platforms/android` 有两个构建脚本，产物**同名同路径**（`target/android/msime-client.apk`），装到设备上也是同一个包名，但内容完全不同：

- `build-apk.sh <已锁定词库目录>` —— 本目录自己的原生 IME 包。Java 宿主来自 `platforms/android/java`，manifest 是 `platforms/android/AndroidManifest.xml`，图标是 `platforms/android/res/drawable/app_icon_*`，不含任何 Tauri/WebView/React。装真机验证本目录的改动用这个。
- `build-client-apk.sh <已锁定词库目录> [abi]` —— Tauri 合包。走 Gradle，从 `apps/desktop/src-tauri/gen/android/` 构建，把原生 IME 和 React 设置界面装进同一个包。

下面「Tauri + React 共享设置合包」一节里的「推荐本地构建入口」只针对需要管理 UI 的场景，不是默认入口。要改、要验、要装本目录的原生宿主，就用 `build-apk.sh`。

两者的资源来源也不同，改图标时尤其要认清：合包的 Gradle 用 `res.setSrcDirs` 覆盖了默认目录，取的是 `src/main/res-msime`、`platforms/android/res` 和 `apps/desktop/src-tauri/icons/android` 三处——`gen/android/app/src/main/res` 下那份 mipmap 不参与构建。启动器图标在合包里来自 `apps/desktop/src-tauri/icons/android`（adaptive icon，五个主题各一个 background 颜色），在原生包里来自 `platforms/android/res/drawable/app_icon_*`。改了一处不等于另一处也改了。

## Tauri + React 共享设置合包

需要管理 UI 时的本地构建入口（不是本目录的默认入口，见上节）：`ANDROID_SDK_ROOT=<SDK绝对路径> bash platforms/android/build-client-apk.sh <已锁定词库目录> [arm64-v8a|x86_64]`。默认 arm64-v8a，产物仍为 target/android/msime-client.apk；需要先完成根目录 pnpm install --frozen-lockfile，准备 JDK 21、Android API 36、build-tools 35、固定 NDK/vcpkg 与 Rust Android target。Gradle 8.14.3 使用官方分发摘要固定，AGP/Kotlin 版本由项目固定。参数 --ci 仅用于 Tauri CLI 的非交互模式，不运行 GitHub CI。

apps/desktop/src-tauri/src/lib.rs 是桌面与移动共用的 Tauri commands/入口，Android 调用同一个 client-core PreferencesStore，指向应用私有 files/bootstrap/state，与 bootstrap 和 IME 监控目录一致。packages/ui 的 React 页没有 Android 副本。生成的 Android 工程已纳入源码，Gradle 直接引用 platforms/android/java、共享图标与暂存的锁定资源；不把原生宿主代码复制到 gen。不要重复执行 tauri android init 覆盖本仓定制。受版本控制的 Gradle 设置会从 `TAURI_ANDROID_DIR` 或 Cargo registry 定位锁定 Tauri Android 工程，合包脚本通过 `cargo metadata --locked` 注入精确路径；生成 Kotlin 绑定、native symlink、构建输出和本机配置仍忽略。

Android“我的”页通过 Rust account session 访问固定的 https://api.msime.app 账号服务。邮箱和手机号验证码、刷新、资料更新、退出及注销请求都在原生宿主内完成，WebView 只接收不含凭据的用户和 provider DTO。会话 JSON 由包私有 Android Keystore AES-GCM 密钥加密后写入 SharedPreferences，密钥和密文不跨应用包共享；请求不跟随重定向，普通 JSON 请求和响应均限制为 1 MiB。日常输入不需要登录，账号登录不会上传本地输入或统计。页面同时提供 Apple 对齐的五种 App 图标选择；Android 通过 launcher `activity-alias` 持久化系统选择，切换只启用目标入口并保留原版回退，不进入设置同步或账号云端数据。

Android“社区”页以 Apple 远端默认分支 `develop` 的固定来源 `MSIME-Apple@9ca823ab40018ced3cb71812503dbc3b94615ac0` 为浏览基线，提供公开皮肤双列列表、原样 UTF-8 搜索、分页去重和详情预览。匿名用户可直接浏览；已有账号会话时请求携带同一 Bearer token，使服务返回“我的作品”和个人评分状态，401 只刷新一次，账号在请求期间变化会废弃结果。登录后可下载：设计以社区 UUID 稳定保存到本地最多 12 项图库，写入前持久化原皮肤和试用记录，主应用重启时恢复未完成试用；详情页支持恢复原皮肤、保留使用及下载后的 1–5 星评分，自己的作品隐藏评分入口。发布侧提供本地命名皮肤上传、公开素材权利确认、失败安全重试、全部/我的作品范围切换和作者下架；下架沿用后端语义，已下载的本地副本保留。Rust transport 对 offset、查询、页长、UUID、文本、评分、发布和完整设计做边界校验，WebView 只接收稳定脱敏错误码。

Apple 的社区资源入口已迁移到同一页的“皮肤 / 词库 / 回复”分类：词库与回复资源支持公开列表、原样 UTF-8 搜索、全部/收藏/我的作品范围、详情预览、收藏、评分、发布新版本和作者下架；词库可显式导入本机词库或在账号版本检查后合并到云词库，回复模板只有用户明确添加后才写入应用私有 `files/CommunityLibrary.json`，由独立 `:ime` 进程重新读取。资源内容、分页、版本、权重、提示词和本地模板库均有大小、数量、控制字符、重复项及稳定错误码边界；发布前要求确认公开权利。账号页提供已发布皮肤、已发布资源和收藏资源的直达入口。

应用首次启动、缺少运行配置时由宿主自己准备词库：两个 launcher（原生宿主的 HomeActivity 与合包的 MainActivity）都调用幂等的 `FirstRunPreparation.startIfNeeded`，已有配置只报告不覆盖。准备中与失败在“键盘”页顶部显示，失败可点击重试；就绪时不占屏幕。原先那个只有一排系统按钮的 SetupActivity 开发页已删除——把唯一的词库准备入口藏在一个用户找不到的脚手架页后面，等于允许键盘停在够不到 Engine 的状态。切换输入法的入口移到“试用键盘”页，系统输入法设置仍在“键盘”页的“系统设置”格。不会自动启用或选择输入法。系统输入法设置入口也可打开共享设置页。Tauri 使用主进程，InputMethodService 使用同 UID 的独立 :ime 进程，通过文件锁和 revision 协作，不依赖设置窗口存活。这样 Tauri 退出最后一个窗口不会结束输入服务；不是通过让隐藏设置窗口常驻来维持输入。

Tauri 合包的“我的”页“云词库”入口打开同包共享云词库面板，继续复用 `packages/ui` 的目录、个人候选、文件和应用流程；入口通过 `msime_mobile_panel=cloud-dictionary` 传给 `MainActivity`，由 WebView 派发受控面板事件。独立原生 APK 没有 WebView，入口会明确提示使用管理界面合包，不伪造一个失效入口。

Tauri 合包的“键盘”页另有“个人词库”入口，打开共享设置页的 `dictionary` 分类，覆盖 Apple 个人词库的查询、编辑、导入和导出；词条校验及 Engine 写入仍由公共 Tauri/Rust 流程负责。入口通过 `msime_settings_page=dictionary` 传给 `MainActivity`，由 WebView 派发受控页面事件；独立原生 APK 会提示使用管理界面合包。

Tauri 合包的 Android“我的”页“关于水杉”入口打开共享 `about` 页面；页面内的使用帮助和反馈继续通过 `help` / `feedback` 分类切换，复用公共版本、隐私、开源、帮助和反馈 UI。独立原生 APK 保留 Android fallback 页面；这样两种构建产物都不会启动不存在的 WebView Activity。

“我的”页的“社区作品”入口打开共享账户页。Android Tauri 宿主现在注入账号命令客户端，复用公共登录、昵称、发布/收藏作品和账号管理流程；原生匿名身份仍只用于无需登录的社区目录浏览，账号会话由 Android 插件安全存储。

共享设置的“屏幕键盘”页面在 Android 上通过 `android_open_keyboard_tryout` 打开原生 `KeyboardTryoutActivity`；Tauri 只提供公共配置和入口，实际输入仍走 Android 原生试用键盘与 `InputConnection`，不在 WebView 内伪造输入法。

本地跑这条命令得到的是开发构建：使用 `target/android/development.keystore` 的开发签名和 versionCode 1，便于覆盖安装同一开发包；开发密钥不得发布，正式签名走发布流程而不是这里。`build-apk.sh` 不是被取代的旧入口，而是本目录原生宿主自己的构建入口，验证本目录改动时用它。

在专用 AVD 上运行 `ANDROID_SDK_ROOT=<SDK绝对路径> bash platforms/android/tests/device/smoke.sh emulator-5580 --settings --statistics --handwriting`：保留原有输入与配置热更新测试，并在真实 Tauri WebView 中操作 React 表单，验证保存、共享 revision、内置与自定义键盘皮肤、重新读取与另一个进程中的实际标点上屏；测试不是直接调用保存 command 代替表单行为。设置套件先确认“我的”入口存在，再选择内置霓虹夜航并打开真实编辑器应用“奶油桃桃”模板，通过 Tauri IPC 对独立命名图库执行新建、重命名、更新、应用和删除，并确认图库写入不会提前修改普通 preferences；随后检查 `custom` 选择及卵石、立体、圆角、纹理字段落盘，并在重绑的 `:ime` 进程中通过皮肤按钮无障碍状态确认实际消费“我的皮肤”。独立 fixture 还验证 Keystore 加密会话的往返、密文不含固定明文 marker、清除与 16 KiB 上限，不向生产账号服务发送验证码。测试前后恢复图库、偏好及 fixture 会话文件。独立控制端还连续两次打开/关闭设置，验证 :ime PID 不变且仍能上屏。统计套件通过真实 InputConnection 与 React 页面验证聚合文件、启停、清空和跨进程读写，固定失败阶段不输出编辑器内容，并恢复测试前文件。手写套件需要网络以首次下载 ML Kit 模型，随后使用合成触摸轨迹验证离线识别与真实 InputConnection 提交；模型已存在时直接验证就绪路径。测试恢复原输入方案和偏好文件，不输出候选或编辑器内容；instrumentation 的强制停止与普通设置窗口关闭分开处理。

移动入口布局依据 [Tauri 移动应用入口约定](https://v2.tauri.app/start/migrate/from-tauri-1/#preparing-for-mobile)。本地观察到最后一个 Tauri 窗口关闭时主进程正常退出，故使用 :ime 隔离；不依赖在同进程中禁止退出后的未验证窗口重建行为。

## 共享设置热更新

新 bootstrap 配置包含绝对路径 `preferences_directory`。输入会话启动后，Android 后台读取该目录的共享 PreferencesStore，之后每秒重试；不在输入主线程等待文件锁，不重叠读取，切换编辑器或结束输入后丢弃旧读取结果并停止旧轮询。已有配置没有目录字段时保留原行为，不猜测其他应用的数据目录。

JNI `loadPreferences` 只读取共享层，快照回到会话主线程后调用同一个 `updatePreferences`；键盘侧写入通过 `savePreferences` 进入同一共享 CAS 边界。revision 校验、组词期间延迟应用和重建失败保护仍在 Rust 中。更新只刷新候选视图，不用空预编辑覆盖现有编辑器内容。相同视图和状态不反复重建键盘控件。密码等直接输入字段不启动配置轮询；禁止个性化学习的编辑器在创建和每次快照应用时均强制关闭学习，同时内存中保留未经隐私覆盖的已接受磁盘快照，避免键盘写入意外永久关闭全局学习设置。

读取或应用失败保留当前输入会话，显示非阻断提示，后续继续重试；不写入默认值覆盖坏文件。共享 React 设置页在上述 Tauri 合包中使用同一个存储。

## 原生库交叉构建

固定 NDK r28c (`28.2.13676358`)、Android API 28 与 vcpkg `ef7dbf94b9198bc58f45951adcf1f041fcbc5ea0`。vcpkg manifest 固定 Boost/fmt/spdlog/SQLite 来源；依赖和构建产物放在忽略的 target 下，不修改 Engine 子模块。需要先自行安装对应 SDK/NDK 和 Rust Android 目标；脚本不自动接受 SDK 许可。

```sh
git clone --depth 1 --branch 2025.06.13 https://github.com/microsoft/vcpkg.git target/tooling/vcpkg
target/tooling/vcpkg/bootstrap-vcpkg.sh -disableMetrics
rustup target add aarch64-linux-android x86_64-linux-android
ANDROID_SDK_ROOT=<SDK绝对路径> bash platforms/android/build-native.sh arm64-v8a
ANDROID_SDK_ROOT=<SDK绝对路径> bash platforms/android/build-native.sh x86_64
```

可用 `MSIME_VCPKG_ROOT`、`MSIME_ANDROID_NDK` 指定绝对路径。脚本校验固定版本后安装锁定依赖，构建 release Rust/C++ 宿主和 JNI，SQLite 静态链接；产物为 `target/android/jniLibs/<abi>/{libmsime_host_api.so,libmsime_android.so,libc++_shared.so}`。验证脚本检查 ELF 架构、16 KB LOAD 对齐、动态依赖白名单与宿主/JNI 导出。NDK 和 vcpkg 声明复制到 `target/android/notices/<abi>`，正式分发还需汇总 Rust/Engine 与词库许可材料。

提供 arm64-v8a 与 x86_64 两条构建路径；不提供 32 位 ABI，也没有 Windows 构建脚本。原生库本身不是 APK，需用下述脚本打包。

## 开发 APK 与首次准备

本地构建：`ANDROID_SDK_ROOT=<SDK绝对路径> bash platforms/android/build-apk.sh <已锁定词库目录>`。需要前述 NDK/vcpkg/JDK/Rust 工具及 zip；脚本先通过共享 ResourceStore 校验资源，再构建双 ABI 库，由 `platforms/android/gradle-app` 的 `assembleRelease` 产出未签名 APK（AndroidX 与 Material 是 AAR，资源合并和 R 类生成必须交给 Gradle/AGP），最后用 SDK 的 zipalign/apksigner 对齐、签名并验证 `target/android/msime-client.apk`。APK 随包带锁定词库、原生依赖声明和 `LICENSE`；签名前进行 16 KB zip 对齐，签名后再验证。

开发密钥在忽略的 `target/android/development.keystore`，不得用于正式发行；清理该文件会改变后续开发签名，不能直接覆盖安装由旧密钥签名的包。构建临时文件留在 target/android 便于排查，不触碰任何设备。

用户打开启动页并点击“准备词库”后，后台任务在私有目录解包资源，调用共享 Rust/C++ 校验与工作数据准备，成功后通过 AtomicFile 发布配置。已有配置一律不覆盖，失败可重试；不支持在线升级已运行的词库。解包和工作词库复制需要额外存储空间。启动页只提供手动进入系统设置/选择器的按钮，不自动启用或切换输入法。

APK 包结构、双 ABI、启动 Activity、IME 声明、签名与对齐均由脚本自身校验；API 35 arm64 专用模拟器覆盖安装、首次准备、已有配置不覆盖，以及软键盘通过系统 InputConnection 上屏、退格和密码直接输入。工具契约参考 [d8](https://developer.android.com/tools/d8)、[zipalign](https://developer.android.com/tools/zipalign) 与 [apksigner](https://developer.android.com/tools/apksigner)。

## 专用模拟器验收

预先安装 `system-images;android-35;default;arm64-v8a`，在一个终端运行 `ANDROID_SDK_ROOT=<SDK绝对路径> bash platforms/android/tests/device/start-emulator.sh`。脚本在忽略的 target/android/avd-home 中创建 msime-client-test，固定 emulator-5580，不使用已有个人 AVD；目标端口属于其他 AVD 时拒绝运行。需要额外磁盘空间，测试结束后应停止该专用模拟器以释放内存；脚本不会下载系统镜像，也不自动接受 SDK 许可。

完成上述 APK 构建、等待系统启动后，在另一终端运行 `ANDROID_SDK_ROOT=<SDK绝对路径> bash platforms/android/tests/device/smoke.sh emulator-5580`。该命令会安装 `app.msime.android` 开发包和独立合成编辑器，准备资源，并在专用 AVD 上启用和选择 MSIME；拒绝非模拟器或名称不符的设备，不对现有真机执行操作。测试不会清空应用数据，重复执行覆盖已有配置路径而非重新模拟首次安装。

独立 instrumentation 读取编辑器与输入法的交互窗口，等待窗口稳定后重新定位并注入触摸，断言“你好”提交、退格、“直接输入”状态和密码框字符长度；不记录编辑器原文。普通 uiautomator dump 只用于准备 Activity，不能用它缺少输入法节点推断键盘未显示。APK fixture 不随产品打包。

同一 smoke 脚本还执行同开发签名的 PreferencesDeviceSmoke；instrumentation 通过独立的 `app.msime.client.test` 测试包，在 `app.msime.android` 的私有测试目录原子发布合成设置，无需给产品增加导出的测试写接口。目标进程重启后重新绑定专用 AVD 的 IME；测试组词延迟、提交保留、页大小与标点生效、损坏文件保护以及恢复重试。结束时恢复原偏好文件（原本不存在则删除测试文件），不清空资源和用户数据。该测试必须经专用 AVD 检查的 smoke 脚本执行，不安装在个人设备。

KeyboardHeightDeviceSmoke 通过键盘内真实无障碍调节动作验证 -12、0 和 +48 dp 档位：正负调整必须改变实际字母键边界，调节和保存期间已有 Engine 组合不得丢失，保存值在 IME 进程重启后必须继续生效。`--settings` 还让 SettingsDeviceSmoke 在真实 React WebView 中修改并保存同一高度字段，再由独立输入法进程消费；两项测试结束时都恢复原偏好文件。

共享核心单元测试也可在该 AVD 实际执行（从仓库根运行，以下工具链为 macOS 主机）：

```sh
export ANDROID_SDK_ROOT=<SDK绝对路径>
android_toolchain="$ANDROID_SDK_ROOT/ndk/28.2.13676358/toolchains/llvm/prebuilt/darwin-x86_64/bin"
CC_aarch64_linux_android="$android_toolchain/aarch64-linux-android28-clang" \
AR_aarch64_linux_android="$android_toolchain/llvm-ar" \
CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$android_toolchain/aarch64-linux-android28-clang" \
CARGO_TARGET_AARCH64_LINUX_ANDROID_RUNNER="bash $PWD/platforms/android/tests/device/run-core-test.sh" \
cargo test -p msime-client-core --lib --target aarch64-linux-android --locked
```

8 项测试在 Android 上通过，包含独立 PreferencesStore 的并发冲突和资源失败保护。Android 标准库文件锁不可用，共享锁封装在该目标使用 rustix 安全 flock，其他目标保留标准库锁；不放宽共享核心的 unsafe 禁令。启动页和软键盘均应用系统栏 inset，避免操作被 ActionBar 或导航栏遮挡。

系统契约参考：[InputMethodService](https://developer.android.com/reference/android/inputmethodservice/InputMethodService) 与 [InputConnection](https://developer.android.com/reference/android/view/inputmethod/InputConnection)。
