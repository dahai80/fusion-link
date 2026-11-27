# fusion-link（设备侧连接守护）

Fusion 本地优先生态的连接守护进程，是 fusion-mlx.cn 云端控制台的设备侧另一半：运行在用户本地 Mac 上的常驻守护，向云端 broker 发起**纯出站** WSS 连接，上报设备健康状态，把云端发起的任务转交给本地 `fusion-gateway` 执行，并将进度实时回传浏览器。

一句话：**fusion-link = 本地 Fusion 系统在 fusion-mlx.cn 上的"在线状态"与"可触达性"。**

- PRD：monorepo 根目录 `architecture/fusion-link-prd-1125.md`
- M1 MVP 范围：FR-1 / FR-2 / FR-3 / FR-4 / FR-6
- 云端 broker：独立二进制，位于 `fusion-ops` 仓库（PRD §11.1）

## 架构

```
[浏览器] --HTTPS--> [fusion-site / VPS]
                          | broker（VPS，仅 127.0.0.1，nginx 不暴露）
                          ^ WSS（设备主动出站，token + 设备指纹鉴权）
                      [fusion-link 守护（本仓库，本地 Mac，fusion-sv 托管）]
                          | 仅本地回环 HTTP
                      [fusion-gateway :11432 -> mlx/rag/artifacts/office（全本地）]
```

零公网入站：守护进程只拨出连接。任务全部本地执行；默认只有摘要、进度和可选的产物预览离开本机，文件全文永不上云。

## WSS 协议（PRD §7）

信封：`{v: 1, type, ts, device_id, seq, payload}`，全 JSON，seq + ack 保证可靠有序。

| type | 方向 | 说明 |
|---|---|---|
| `hello` / `hello_ack` | 上行 | 注册：凭证、版本、能力集 |
| `heartbeat` | 上行 | 30s 心跳 + 本地服务健康快照 |
| `task.dispatch` | 下行 | 任务指令（id、类型、参数、TTL） |
| `task.event` | 上行 | 状态机事件：pending / running / awaiting_local_confirm / succeeded / failed / cancelled |
| `task.result` | 上行 | 结果摘要 + 产物引用 |
| `config.update` | 下行 | 策略/配额变更 |
| `ack` | 双向 | 确认收到 `ack_seq` 及之前的消息 |
| `error` | 双向 | `E_AUTH` / `E_QUOTA` / `E_OFFLINE` / `E_UNSUPPORTED` / `E_LOCAL_FAIL` |

所有上行消息写入持久化 outbox（`~/.fusion-link/outbox.json`），broker 确认前反复重发——断线、崩溃后事件不丢（FR-6）。因当前 broker 不 ack 心跳与任务事件，`hello_ack` 时清空心跳条目，日志上限 500 条（最旧先淘汰）。重连采用指数退避 + 抖动（1s 起步，60s 封顶）。

## 安装

**Homebrew**（macOS / Linux）：

```bash
brew install dahai80/tap/fusion-link
```

**pip**（以 Python wheel 形式分发编译好的二进制）：

```bash
pip install fusion-link
```

**源码构建**：需要 Rust 1.85+（edition 2024）。

```bash
cargo build --release
```

每个 [GitHub release](https://github.com/dahai80/fusion-link/releases) 附带 `aarch64/x86_64-apple-darwin` 与 `x86_64-unknown-linux-musl` 预编译二进制（`.tar.gz` + sha256）。

## 构建与测试

```bash
cargo build --release
cargo test                                    # 单元 + 集成（mock broker / mock gateway）
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
```

## 使用

```bash
# 1. 绑定设备（绑定码来自 fusion-mlx.cn 控制台）
fusion-link bind <code>

# 2. 查看状态
fusion-link status

# 3. 运行守护（前台；start.sh 管理 pidfile + 日志）
./start.sh start
./start.sh status
./start.sh log
./start.sh stop
```

默认任务路由：`shell.chat` -> `POST /api/v1/shell/chat/completions`；云端语义 LLM 类型 `summarize` / `generate` / `analyze`（云端控制台派发）映射到同一端点，平铺的 `prompt` 参数会被改写为 OpenAI `messages` 请求体；办公路由 `office.spreadsheet` -> `/shell/office-api/v1/xlsx/parse`、`office.document` -> `/shell/office-api/v1/docx/write`、`office.slides` -> `/shell/office-api/v1/pptx/write`（均经 `gateway_url`，300 秒超时）。不支持的任务类型以 `E_UNSUPPORTED` + 原因明确拒绝（FR-3）。

网关任务鉴权：守护进程用设备凭证经 `POST {gateway_url}/api/v1/link/ticket`（仅回环，fusion-gateway #717 服务票据面）换取 ≤15 分钟短时票据，并以 `Authorization: Bearer <ticket>` 附带。若网关未启用票据面，回退到 `gateway_token`（或无鉴权）。任务中收到 401 会作为可见任务失败上报——绝不静默重试。

Multipart 任务：`params` 含 `"_multipart": true` 时，守护进程发送 `multipart/form-data` 而非 JSON（office-api 文件端点要求）。`file_b64`（base64）作为 `file` 部分（文件名取 `file_name`，MIME 取 `file_mime`）；其余参数转为表单字段，非字符串值 JSON 编码。

## 配置

`~/.fusion-link/config.toml`（所有键可选；文件存在时以文件为准）。环境变量覆盖：`FUSION_LINK_BROKER_URL`、`FUSION_LINK_BIND_URL`、`FUSION_LINK_GATEWAY_URL`、`FUSION_LINK_GATEWAY_TOKEN`、`FUSION_LINK_STATE_DIR`、`FUSION_LINK_HEARTBEAT_SECS`。

| 键 | 默认值 |
|---|---|
| `broker_url` | `wss://fusion-mlx.cn/link/ws` |
| `bind_url` | `https://fusion-mlx.cn/api/link/bind` |
| `gateway_url` | `http://127.0.0.1:11432` |
| `heartbeat_secs` | `15`（broker 读超时 30s，心跳必须小于它） |
| `state_dir` | `~/.fusion-link` |
| `task_routes` | `shell.chat` + 3 条 LLM 别名 + 3 条办公路由 |
| `probes` | mlx :11434、gateway :11432、rag :11436、artifacts :11451、office :11485、office-api :11492 |

健康快照只含服务名 + 布尔状态（NFR-2）；心跳小于 1KB（NFR-3）。

## 凭证存储

设备凭证（`device_id` + token + 指纹）通过 `security` CLI 存入 macOS 登录钥匙串（service 名 `fusion-link`）。指纹取自 `IOPlatformUUID`。`fusion-link unbind` 删除凭证与本地 outbox 状态。

## 托管

`start.sh` 遵循 fusion-supervisor 托管契约（NFR-6）：pidfile 在 `~/.fusion-link/fusion-link.pid`，日志在 `~/.fusion-link/fusion-link.log`。注册纳管后由 `fusion-sv` 统一启停与崩溃恢复。

## 状态

M1 MVP（2026-11）：设备侧 FR-1/2/3/4/6 已实现。Broker（fusion-ops 仓库）与控制台页面为独立交付物；默认 `broker_url` 为占位值，broker 端点上线前不可用。M2 范围（FR-5/7/8/11/12）见 PRD 里程碑。

## 许可证

Apache-2.0
