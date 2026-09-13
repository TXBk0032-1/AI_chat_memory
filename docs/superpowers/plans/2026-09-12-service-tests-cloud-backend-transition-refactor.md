# `service/tests/cloud_backend_transition.rs` 重构与底座下沉实施计划

> **执行纪律前置说明：**
> 1. **时机门禁**：本重构计划严禁与正在进行的业务缺陷修复混行。必须待当前分支既定修复全部验收落地闭环后，作为独立的重构分支/原子提交单独推进。
> 2. **数据纪律**：本计划内测试名称、行号与计数均经专用命令脚本解析提取（见附注输出）。实现过程中严禁擅自重命名测试、删减用例或修改断言语义。
> 3. **基线快照与动态解析声明**：本计划中给出的 426 个测试总数与 55 行映射表行号，均为制定本计划时的静态基线快照。时机门禁明确规定重构在业务缺陷修复落地后启动，届时全库测试总数大概率新增、代码行号必然发生相对偏移。执行步骤 1.1 时必须以当次运行 `cargo test --lib -- --list` 动态捕获的裸测试名集合作为基准快照，测试函数名称及其关注点分类归属才是不可变更的绝对契约。
> 4. **计划文档去向规范**：在当前 `fix/audit-confirmed-issues` 缺陷修复分支中，本计划由 `.git/info/exclude` 本地精确排除，严禁污染当前工作树；待后续开启独立的 `refactor/service-tests` 重构分支时，在首个提交（Commit 0）中从 exclude 中解除并正式 `git add` 入库，作为重构工程的权威跟踪规格。

---

## 架构诊断与重构本质

原文件 `cloud_backend_transition.rs`（4,230 行，55 个用例）的痛点本质由三个相互交织的根因导致：

1. **病 1：冗余 —— 根源是“缺少领域状态词汇”，而非“缺少工具函数”**
   - 53 处手写 `CloudSyncSettings { ... }`、25 处手写 `VaultIdentity { format_version: 2, ... }`、18 处手写 `load_or_create_vault + VaultDocument::active`；
   - 现有 7 个旧 fixture 是按“调用方”而非“领域状态”抽象（如 `configured_s3_service_for_sync_guard_tests` 是“给 guard 用的”，而不是“处于什么状态”）；
   - 治本之策：按**远端领域状态**抽象，归纳为 7 种核心具名状态（空远端、明文活跃、加密活跃、冻结中、发布中、released-v1 无链归档、集群加入）。
2. **病 2：混杂 —— 三层抽象杂糅在单个测试函数中**
   - 单个 52 行的测试中，35 行在手搓底层协议细节（如 `begin_generation_freeze_owned` 传递 8 个位置参数），10 行在写不变量断言，真正测试核心意图仅占 3 行；
   - 内部 API 签名（如 8 参数函数）焊死在数十个测试中，内部稍作改动便引发大面积测试编译崩溃；
   - 治本之策：建立**不变量断言领域词汇**，将协议样板下沉至底座，将测试函数信噪比提升至 90% 以上。
3. **病 3：脆弱 —— 具体的、会真实引发故障的隐患**
   - **临时目录残留（从从不清理转为尽力清理）**：`_data_dir` 下划线丢弃 22 处，现有 9 处 `remove_dir_all` 全部集中在 4 个 move 测试中，其余 51 个用例从不清理，导致历史积累多达 1,900+ 临时目录。由于 SQLite 连接池是异步惰性释放，Windows 平台同步 Drop 中执行 `remove_dir_all` 存在句柄竞争静默失败风险，因此方案定级为“**尽力而为清理（Best-Effort RAII）+ 步骤 1.1 存量预清场兜底**”；
   - **人肉命名漂移与心智负担**：测试散落着 `svc6/svc7/svc7-fail` 等硬编码字符串。虽然每个测试拥有独立的 `TestS3` 实例与本地端口，不存在跨用例网络/存储碰撞风险，但人肉硬编码带来了极高维护与重命名心智负担；
   - **13 处全量 `serde_json::to_value(整个 settings)` 比对**：给 `AppSettings` 新增任何非云同步字段（如 theme），语义瞬间漂移为脆弱的全量断言；
   - 治本之策：引入 **`TempDataDir` RAII 守卫**、**`auto_prefix` 消除人肉命名漂移**、以及**仅针对 `cloud_sync` 子树的比对断言**。

---

## 目标与基线数据

- **重构对象**：`app/src-tauri/src/service/tests/cloud_backend_transition.rs`（制定时实测 **4,230 行**，共 **55 个测试用例**）。
- **全库 lib 验证基准**：制定时 `cargo test --lib -- --list | Select-String ": test$"` 提取裸测试名精确为 **426 个测试**，其中该文件占 **55 个**。
- **物理减重杠杆（实测数据）**：
  - 消除 **53 处** 手写 `CloudSyncSettings { ... }` 样板代码；
  - 统一收敛 **19 处** `backend_from_store` 构造入口；
  - 消除 **5 处** 手写凭据循环注入（实测行号：194、266、361、762、1535）；
  - 统一 **13 处** `serde_json::to_value(settings_before).unwrap()` 全量比对与 **4 处** 手写 `vault.json` before/after 比对；
  - 收口 **5 处** 手写 `list_depth_one` 世代过滤断言；
  - 封堵 **22 处** `_data_dir` 从不清理的盲区；
  - **预计净减代码行数**：800 ~ 1,200 行。
- **执行时间杠杆（KDF 加速边界、真实派生成本与收益定级）**：
  - **现状实测**：单用例 `restart_reconciles_a_committed_generation_before_selecting_credentials` 实测耗时 **39.43s ~ 44.76s**。
  - **派生成本实测基准（Debug Profile）**：
    - 生产参数（64 MiB / t=3）：单次耗时约 **1.7s ~ 5.2s**（均值约 2.5s）；
    - 快速参数（8 MiB / t=1）：单次耗时约 **0.3s**。
  - **收益定级与设计权衡（恪守零业务逻辑变动）**：
    - 坚持“Zero Logic Change”原则，**坚决不修改生产代码注入测试钩子**；
    - 生产代码内部 `create_encrypted_vault_protection`（`app/src-tauri/src/service.rs:1107-1123`）硬编码走生产参数并在 `kdf_limit()` 串行控制下运行；
    - 测试侧使用 `sync/vault.rs:93` 的 `pub(crate) fn encrypted_with_config`，**仅能加速测试自身 Fixture 装配与播种时的 1~2 次派生**（理论最多节省 3~5 秒）；
    - 剩余 30 秒+ 耗时属于生产重启对账内部派生、AWS SDK 客户端握手、本地 HTTP 路由及 SQLite I/O 等固有开销；
    - **收益定级**：修正为“**秒级收益（≤5s），阶段二实测标定，不设硬性时间目标**”；
    - **日常开发迭代提速手段**：日常开发与回归验证必须通过指定测试名过滤子集执行（例如 `cargo test --lib restart_reconciles_a_committed_generation` 或 `cargo test --lib service::tests::restart_reconcile`），获得亚秒/秒级局部反馈，全量慢测试仅由阶段门禁与 CI 守门。
  - **生产参数安全防线说明**：
    - `sync/crypto.rs:99` 的单测使用的是 8 MiB `test_kdf`，仅用于验证密码学加解密向量；
    - 真正对生产级 64 MiB / t=3 默认参数进行断言与覆盖的是 `sync/vault.rs:1221` 起的单元测试（`encrypted_vault_metadata_verifies_the_candidate_passphrase`）。集成测试切到 fast KDF 播种后，生产默认参数的契约仍由 `sync/vault.rs` 的单元测试全权守住。

---

## 架构蓝图：两级底座与模块布局

```text
app/src-tauri/src/
├── test_support/                             # [NEW] Crate 级共享测试底座 (#[cfg(test)] pub(crate))
│   └── mod.rs                                # 对齐并联动 sync/engine/tests.rs
│                                             #  - test_s3_backend(server: &TestS3)
│                                             #  - test_protector(passphrase: &str)
│                                             #  - initialize_test_vault(...)
│                                             #  - assert_generation_layout(...)
│                                             #  - fast_kdf_config() (8 MiB / 1 迭代)
│
├── service.rs                                # 内联 4 个纯配置策略校验单测 (原 2823-2891 行，测 2231/2265 行函数)
└── service/
    └── tests/
        ├── mod.rs                            # 注册 12 个子模块
        ├── support.rs                        # [NEW] Service 级三层领域底座 (CloudFixture / TempDataDir)
        ├── credentials_rollback.rs           # [NEW] 活体故障注入两阶段回滚 (2 个用例)
        ├── switch.rs                         # [NEW] 后端存储切换与双端迁移 (3 个用例，含专用 WebDAV 拓扑)
        ├── encryption.rs                     # [NEW] 加解密开启/关闭与密码轮转 (6 个用例)
        ├── restart_reconcile.rs              # [NEW] 崩溃/重启时未决状态对账与抢修 (7 个用例)
        ├── guards.rs                         # [NEW] 冲突拦截与错误密码防护 (10 个用例)
        ├── rewrite.rs                        # [NEW] 归档全量重写与失败清理 (5 个用例)
        ├── released_v1.rs                    # [NEW] 遗留 v1 无链归档兼容与淘汰 (3 个用例)
        ├── baseline_sync.rs                  # [NEW] 首次同步基线与远端接管 (2 个用例)
        ├── device_management.rs              # [NEW] 远端设备前缀物理删除与游标维护 (3 个用例)
        ├── data_dir_migration.rs             # [NEW] 本地数据目录原子迁移与读写拦截 (4 个用例)
        ├── sync_gate_concurrency.rs          # [NEW] 本地写入在世代维护期间的互斥 (2 个用例)
        └── sync_scheduler.rs                 # [NEW] 调度器优先级与状态映射单测 (4 个用例)
```

**用例总数精确核对**：
`service/tests/` 下 12 个子文件共 51 个集成用例 + `src/service.rs` 内联 4 个策略单测 = **55 个用例**。

---

## 55 个测试用例 1:1 全量映射表

> 注：下表行号由制定本计划时通过 Node 脚本解析 `cloud_backend_transition.rs` 中 `fn` 声明所在行生成，供对照原文件定位参考。重构执行时以测试函数名与归属模块为准：

| # | 当前行号(快照) | 测试函数名 | 归属模块 | 关注点分类 |
| :---: | :---: | :--- | :--- | :--- |
| 1 | 259 | `s3_credential_update_rolls_back_an_atomic_bundle_write_failure` | `credentials_rollback.rs` | 活体注入：两阶段凭据写入回滚 |
| 2 | 357 | `settings_write_failure_restores_every_s3_credential_and_the_active_draft` | `credentials_rollback.rs` | 活体注入：配置持久化失败还原凭据 |
| 3 | 450 | `backend_switch_defers_generation_replay_without_rewriting_local_versions` | `switch.rs` | 后端切换：延迟重放且不改本地版本 |
| 4 | 557 | `webdav_to_s3_switch_publishes_live_sessions_and_tombstones_without_touching_webdav` | `switch.rs` | 后端迁移：WebDAV 转 S3 不碰源端 (双端拓扑) |
| 5 | 817 | `sync_password_change_reads_old_chain_and_commits_new_encrypted_generation` | `encryption.rs` | 加密轮转：密码变更与新世代提交 |
| 6 | 917 | `restart_reconciles_a_committed_generation_before_selecting_credentials` | `restart_reconcile.rs` | 重启对账：已提交新世代先于凭据恢复 |
| 7 | 1017 | `restart_rolls_back_an_expired_pending_building_freeze` | `restart_reconcile.rs` | 重启恢复：过期 building 冻结状态回滚 |
| 8 | 1103 | `restart_activates_an_expired_pending_ready_freeze` | `restart_reconcile.rs` | 重启恢复：过期 ready 冻结状态激活 |
| 9 | 1202 | `restart_finishes_a_pending_head_publication_before_reconciling_credentials` | `restart_reconcile.rs` | 重启恢复：补完未决的 Head 发布 |
| 10 | 1321 | `expired_pending_freeze_with_a_wrong_active_passphrase_does_not_touch_remote_state` | `restart_reconcile.rs` | 状态守卫：错误密码不破坏远端冻结 |
| 11 | 1405 | `fresh_pending_publishing_with_a_wrong_active_passphrase_does_not_touch_remote_state` | `restart_reconcile.rs` | 状态守卫：错误密码不破坏远端发布 |
| 12 | 1556 | `sync_rejects_remote_plain_when_local_encryption_is_persisted` | `guards.rs` | 策略防线：本地加密拒绝远端明文 |
| 13 | 1596 | `sync_wrong_passphrase_does_not_recover_expired_frozen_vault` | `guards.rs` | 策略防线：错误密码拒绝抢修冻结 |
| 14 | 1650 | `sync_wrong_passphrase_does_not_recover_publishing_vault` | `guards.rs` | 策略防线：错误密码拒绝抢修发布 |
| 15 | 1726 | `sync_rejects_plain_active_from_a_frozen_recovery_reread_without_mutation` | `guards.rs` | 状态防线：重读出现明文时拒绝篡改 |
| 16 | 1801 | `verified_vault_rejects_changed_identity_from_a_frozen_recovery_reread` | `guards.rs` | 标识防线：重读出现标识漂移时拒绝恢复 |
| 17 | 1932 | `saving_settings_rejects_remote_plain_when_local_encryption_is_persisted` | `guards.rs` | 保存防线：本地已加密拒绝保存明文远端 |
| 18 | 1999 | `joining_existing_vault_with_wrong_passphrase_does_not_recover_expired_frozen_vault` | `guards.rs` | 集群加入：错误密码不抢修远端冻结 |
| 19 | 2052 | `joining_existing_vault_with_wrong_passphrase_does_not_recover_publishing_vault` | `guards.rs` | 集群加入：错误密码不抢修远端发布 |
| 20 | 2125 | `rewrite_rejects_remote_plain_when_local_encryption_is_persisted` | `rewrite.rs` | 归档重写：本地已加密拒绝重写明文远端 |
| 21 | 2176 | `enabling_encryption_reads_the_old_plain_chain_and_commits_an_encrypted_generation` | `encryption.rs` | 开启加密：拉取明文旧链并提交加密新世代 |
| 22 | 2226 | `disabling_encryption_reads_the_old_chain_and_commits_a_plain_generation` | `encryption.rs` | 关闭加密：拉取加密旧链并提交明文新世代 |
| 23 | 2289 | `credential_failure_before_rotation_leaves_the_remote_generation_untouched` | `encryption.rs` | 容灾防线：轮转前凭据报错不触碰远端 |
| 24 | 2359 | `settings_write_failure_after_rotation_keeps_the_active_generation_and_new_credentials` | `encryption.rs` | 容灾防线：轮转后配置落盘失败保留活跃状态 |
| 25 | 2436 | `matching_remote_passphrase_corrects_the_credential_without_rotating_generation` | `encryption.rs` | 凭据自愈：输入正确密码纠正本地且不轮转 |
| 26 | 2493 | `connection_test_prepares_a_draft_without_persisting_local_or_remote_state` | `guards.rs` | 连接探测：测试草稿隔离（严禁产生副作用） |
| 27 | 2587 | `joining_an_existing_vault_rejects_plain_and_encrypted_policy_mismatches` | `guards.rs` | 集群加入：明文/加密策略不匹配坚决拒绝 |
| 28 | 2680 | `cloud_error_kinds_map_to_distinct_runtime_states` | `sync_scheduler.rs` | 纯内存单测：错误种类到运行时状态映射 |
| 29 | 2709 | `production_scheduler_coalesces_priority_and_uses_bounded_delays` | `sync_scheduler.rs` | 纯内存单测：调度器触发合并与延迟上限 |
| 30 | 2732 | `production_scheduler_pauses_auth_retries_until_manual_trigger` | `sync_scheduler.rs` | 纯内存单测：鉴权错误暂停自动重试 |
| 31 | 2751 | `production_scheduler_retries_only_offline_with_capped_jitter` | `sync_scheduler.rs` | 纯内存单测：仅离线触发抖动重试退避 |
| 32 | 2785 | `enabling_cloud_sync_queues_the_seeded_baseline_for_automatic_sync` | `baseline_sync.rs` | 初次同步：开启同步自动将基线放入队列 |
| 33 | 2824 | `backend_switch_rotates_identity_once` | `service.rs (策略单测)` | 纯函数单测：切换后端触发一次标识轮转 |
| 34 | 2842 | `changing_remote_location_rotates_identity_and_requests_a_new_baseline` | `service.rs (策略单测)` | 纯函数单测：换存储桶触发新基线请求 |
| 35 | 2858 | `unverified_new_or_changed_connection_cannot_be_enabled` | `service.rs (策略单测)` | 纯函数单测：未校验成功的连接禁止启用 |
| 36 | 2881 | `enabled_legacy_webdav_connection_remains_usable` | `service.rs (策略单测)` | 纯函数单测：已启用的旧 WebDAV 保持可用 |
| 37 | 2893 | `import_waits_for_generation_maintenance_before_mutating_local_state` | `sync_gate_concurrency.rs` | 并发互斥：会话导入等待世代维护锁释放 |
| 38 | 2942 | `delete_waits_for_generation_maintenance_before_mutating_local_state` | `sync_gate_concurrency.rs` | 并发互斥：会话删除等待世代维护锁释放 |
| 39 | 2982 | `backend_switch_waits_for_running_sync_before_changing_configuration` | `switch.rs` | 并发互斥：配置变更等待当前在途同步完成 |
| 40 | 3040 | `remove_cloud_device_record_deletes_only_the_requested_remote_prefix` | `device_management.rs` | 设备运维：删除远端设备仅清理目标前缀 |
| 41 | 3114 | `sync_adopts_a_remote_generation_and_replays_the_local_baseline_once` | `baseline_sync.rs` | 世代接管：发现远端新世代单次重放本地基线 |
| 42 | 3253 | `sync_recovers_an_abandoned_frozen_vault_before_publishing` | `restart_reconcile.rs` | 故障抢修：发布前自动抢修废弃的冻结世代 |
| 43 | 3333 | `rewrite_cloud_archive_cleans_partial_generation_when_baseline_publish_fails` | `rewrite.rs` | 归档重写：基线发布失败回滚并清理半成品 |
| 44 | 3435 | `rewrite_cloud_archive_keeps_activated_generation_when_followup_sync_fails` | `rewrite.rs` | 归档重写：新世代已激活则容忍后续同步报错 |
| 45 | 3533 | `rewrite_cloud_archive_switches_the_persisted_and_remote_generation` | `rewrite.rs` | 归档重写：正常流程原子切换本地与远端世代 |
| 46 | 3618 | `released_v1_archive_bootstraps_plain_despite_stale_encryption_setting` | `released_v1.rs` | v1 兼容：遇到明文历史归档自动纠偏配置 |
| 47 | 3655 | `released_v1_compatibility_fences_encryption_rotation` | `released_v1.rs` | v1 兼容：未退役兼容模式前阻断加密轮转 |
| 48 | 3690 | `rewrite_cloud_archive_explicitly_retires_released_v1_compatibility` | `released_v1.rs` | v1 兼容：全量重写归档显式标记兼容退役 |
| 49 | 3732 | `move_data_directory_rejects_writes_until_restart` | `data_dir_migration.rs` | 目录迁移：进入迁移状态后拒绝一切新写入 |
| 50 | 3795 | `move_data_directory_rechecks_the_destination_inside_the_sync_gate` | `data_dir_migration.rs` | 目录迁移：进入同步互斥锁后二次检查目标 |
| 51 | 3831 | `move_data_directory_publishes_redirect_for_old_readers` | `data_dir_migration.rs` | 目录迁移：原目录写入重定向 marker 阻断 MCP |
| 52 | 3893 | `move_data_directory_leaves_no_marker_when_the_move_fails` | `data_dir_migration.rs` | 目录迁移：迁移快照失败不留悬空 marker |
| 53 | 3927 | `rewrite_cloud_archive_surfaces_and_marks_a_failed_local_generation_commit` | `rewrite.rs` | 归档重写：本地持久化世代失败显式暴露错误 |
| 54 | 4033 | `remove_cloud_device_record_refreshes_devices_without_faking_sync_success` | `device_management.rs` | 设备运维：无设备变动刷新状态不伪造同步成功 |
| 55 | 4169 | `remove_cloud_device_record_reports_cursor_cleanup_failures` | `device_management.rs` | 设备运维：游标清理异常如实向调用方汇报 |

---

## 实施步骤与两阶段验证协议

### 阶段一：纯文件物理拆分（Zero Logic Change）

- [ ] **步骤 1.1：清理执行环境残留并建立当次基线快照**
  在执行阶段一前，先清场存量临时目录（防 PID 碰撞导致的 UNIQUE 冲突与假失败），随后以当次动态执行结果作为基线：
  ```powershell
  # (起始 CWD: 仓库根目录)
  # 0. 清理执行环境的历史残留，防止 PID 复用导致 UNIQUE 假失败 (存量 ai-chat-memory-* / acm-path-*)
  Remove-Item "$env:TEMP\acm-path-*", "$env:TEMP\ai-chat-memory-*" -Recurse -Force -ErrorAction SilentlyContinue

  # 1. 进入 app/src-tauri 提取当次动态裸测试名基线
  Push-Location app/src-tauri
  try {
      $env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
      $env:RUSTUP_TOOLCHAIN = "1.97.0"
      cargo test --lib -- --list | Select-String ": test$" | ForEach-Object {
          if ($_ -match '::([^:]+): test$') { $matches[1] }
      } | Sort-Object | Set-Content ../../artifacts/before_tests.txt
      $baselineCount = (Get-Content ../../artifacts/before_tests.txt).Count
      Write-Host "Captured dynamic baseline test count: $baselineCount (baseline snapshot was 426)"
      if ($baselineCount -lt 426) { throw "Baseline count decreased unexpectedly: got $baselineCount, expected at least 426" }
  } finally {
      Pop-Location
  }
  ```

- [ ] **步骤 1.2：创建目标文件骨架并原样搬移**
  - 创建 `app/src-tauri/src/service/tests/` 下对应的 **12 个子测试文件** 与 `support.rs`；
  - 严格按照 55 行映射表，将对应的测试函数及私有局部辅助函数**原样拷贝**；
  - 在 `app/src-tauri/src/service/tests/mod.rs` 中注册 12 个新子模块，移除原 `cloud_backend_transition.rs`；
  - 4 个策略函数单测（#33~#36）迁入 `app/src-tauri/src/service.rs` 内部（测试 `prepare_cloud_sync_transition` 与 `validate_cloud_sync_update`）。注意：由于模块层级变更为宿主文件内部，其 `super::` 依赖需调整为同模块内部直接引用。

- [ ] **步骤 1.3：阶段一绝对门禁校验（编译 + Clippy + 裸名零差异）**
  拆分文件重建 use 块极易产生未用导入，必须在阶段一通过 `Push-Location` 执行 Clippy 严查：
  ```powershell
  # (起始 CWD: 仓库根目录，通过 Push-Location 进入 app/src-tauri)
  Push-Location app/src-tauri
  try {
      $env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
      $env:RUSTUP_TOOLCHAIN = "1.97.0"
      # 1. 格式检查
      cargo fmt --check
      # 2. 静态检查 (warnings-denied 契约提前拦截)
      cargo clippy --all-targets --all-features -- -D warnings
      # 3. 提取拆分后的裸测试名快照并比对
      cargo test --lib -- --list | Select-String ": test$" | ForEach-Object {
          if ($_ -match '::([^:]+): test$') { $matches[1] }
      } | Sort-Object | Set-Content ../../artifacts/after_tests.txt
      $diff = Compare-Object (Get-Content ../../artifacts/before_tests.txt) (Get-Content ../../artifacts/after_tests.txt)
      if ($diff) { throw "Test inventory mutated during relocation!`n$($diff | Out-String)" }
      # 4. 运行全量 lib 测试确保全部通过
      cargo test --lib
  } finally {
      Pop-Location
  }
  ```

- [ ] **步骤 1.4：原子提交阶段一**
  在仓库根目录执行，暂存范围覆盖 `src/service.rs`：
  ```powershell
  # (CWD: 仓库根目录)
  git add app/src-tauri/src/
  git commit -m "refactor(service): 按领域主题拆分云同步集成测试文件"
  ```

---

### 阶段二：测试底座下沉与三层领域状态抽象（Deduplication, Decoupling & Hardening）

- [ ] **步骤 2.1：下沉 Crate 级共享底座 `crate::test_support`**
  - 创建 `app/src-tauri/src/test_support/mod.rs`，并在 `lib.rs` 声明 `#[cfg(test)] pub(crate) mod test_support;`；
  - 迁入并统一：
    - `test_s3_backend`（收敛 `sync/engine/tests.rs:920`）；
    - `test_protector`（收敛 `sync/engine/tests.rs:931`）；
    - `initialize_test_vault`（收敛 `sync/engine/tests.rs:952`）；
    - `assert_generation_layout`（收口 5 处 `list_depth_one`）；
    - `fast_kdf_config`（基于 `sync/vault.rs:93` `encrypted_with_config`，提供 8 MiB / 1 迭代配置）。
  - **联动修改**：将 `sync/engine/tests.rs` 的重复定义改为引用 `crate::test_support`。

- [ ] **步骤 2.2：构建 Service 级三层领域底座 (`service/tests/support.rs`)**

  #### 第 1 层：CloudFixture 具名状态构造器与演进
  将 53 处手写配置收敛为 7 种核心远端初始状态与明确租约生命周期（LeaseLifecycle）的演进方法：
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum LeaseLifecycle {
      /// 已过期租约：started_at_ms = 1, lease_expires_at_ms = 2
      Expired,
      /// 活跃有效租约：当前时间戳与正常租期
      Active,
  }

  pub struct CloudFixture {
      pub service: AppService,
      pub server: TestS3,
      pub backend: Arc<dyn CloudBackend>,
      pub settings: CloudSyncSettings,
      data_dir: TempDataDir,
  }

  impl CloudFixture {
      // 提供目录访问器，支持 restart 族定位 settings.json 进行配置破坏与注入
      pub fn data_dir(&self) -> &Path {
          &self.data_dir.path
      }

      // 7 种核心远端初始状态入口
      pub async fn empty(prefix: &str) -> Self;
      pub async fn plain(prefix: &str) -> Self;
      pub async fn encrypted(prefix: &str, passphrase: &str) -> Self;
      pub async fn frozen(prefix: &str, phase: FreezePhase, lease: LeaseLifecycle, reason: &str) -> Self;
      pub async fn publishing(prefix: &str, passphrase: &str, lease: LeaseLifecycle) -> Self;
      pub async fn released_v1(prefix: &str, stale_encryption: bool) -> Self;
      pub async fn joining(prefix: &str, passphrase: &str) -> Self;

      // 状态演进方法（承载过期 vs 活跃租约等核心业务语义）
      pub async fn frozen_by_other_device(self, reason: &str, lease: LeaseLifecycle) -> Self;
      pub async fn publishing_by_other_device(self, lease: LeaseLifecycle) -> Self;
      pub async fn with_local_passphrase(mut self, passphrase: &str) -> Self;
      pub async fn with_credentials(mut self, store: Arc<dyn CredentialStore>) -> Self;
      pub async fn publish_initial_bundle(&self);
  }
  ```

  > **拓扑边界说明（Switch 族专用双端装配）**：
  > `switch.rs` 中的测试 #3 与 #4 属于遗留 WebDAV 源向 S3 目标迁移的跨协议双端拓扑（同时持有 `TestWebDav` 与 `TestS3`），不强行削足适履塞进单端 S3 状态机。在 `support.rs` 中为其保留专用的双端迁移辅助函数 `webdav_to_s3_fixture()`，保持架构正交性。

  #### 第 2 层：不变量断言领域词汇（解耦全局配置变更干扰）
  ```rust
  impl CloudFixture {
      /// 仅精确比对 cloud_sync 配置子树，解耦 AppSettings 中无关字段（如 theme/window）变动的干扰
      pub async fn assert_cloud_config_unchanged(&self);
      /// 精确比对远端 vault 与 head 的 bytes 及 etag，确保零副作用
      pub async fn assert_remote_untouched(&self);
      /// 世代目录物理布局断言
      pub async fn assert_generation_layout(&self, expected: &[&str]);
  }
  ```

  #### 第 3 层：TempDataDir(尽力清理 RAII) 与自动前缀
  ```rust
  pub struct TempDataDir {
      pub path: PathBuf,
  }
  impl Drop for TempDataDir {
      fn drop(&mut self) {
          // 尽力而为清理：Windows 下若遇 SQLite 活跃连接尚未彻底释放，容忍静默失败；
          // 结合步骤 1.1 存量前置清场，构筑双重防线
          let _ = std::fs::remove_dir_all(&self.path);
      }
  }

  // 根据测试名称自动派生短唯一前缀，消灭 svc6/svc7 等人肉硬编码与命名维护负担
  pub fn auto_prefix(test_name: &str) -> String {
      format!("{test_name}_{}", uuid::Uuid::new_v4().simple())
  }
  ```

  #### 经典用例重构范式（52 行 → 9 行）
  以 `sync_wrong_passphrase_does_not_recover_expired_frozen_vault` 为例：
  ```rust
  #[tokio::test]
  async fn sync_wrong_passphrase_does_not_recover_expired_frozen_vault() {
      let fx = CloudFixture::encrypted(&auto_prefix("wrong_pass_frozen"), "correct-passphrase")
          .await
          .with_local_passphrase("wrong-passphrase")
          .await
          .frozen_by_other_device("expired-freeze", LeaseLifecycle::Expired)
          .await;

      let error = fx.service.sync_once_locked(fx.settings.clone()).await.unwrap_err();

      assert!(matches!(error, AppError::Crypto(_)), "{error:?}");
      fx.assert_cloud_config_unchanged().await;
      fx.assert_remote_untouched().await;
  }
  ```

- [ ] **步骤 2.3：逐模块按领域状态替换现有测试样板**
  - 依次在适用的 10 个集成测试子文件中引入 `CloudFixture` 状态构造器与断言 Helper（`sync_scheduler.rs` 纯内存单测与 `data_dir_migration.rs` 本地目录测试保持各自轻量级 setup，不强套 S3 状态机）；
  - `switch.rs` 采用专用双端装配函数 `webdav_to_s3_fixture()`；
  - 消除所有直接下划线丢弃 `_data_dir` 的代码，统一交由 `TempDataDir` 回收。

- [ ] **步骤 2.4：阶段二全量流水线门禁校验**
  ```powershell
  # 2.4.1 验证裸测试名集合未变 (起始 CWD: 仓库根目录，通过 Push-Location 进入 app/src-tauri)
  Push-Location app/src-tauri
  try {
      $env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
      $env:RUSTUP_TOOLCHAIN = "1.97.0"
      cargo test --lib -- --list | Select-String ": test$" | ForEach-Object {
          if ($_ -match '::([^:]+): test$') { $matches[1] }
      } | Sort-Object | Set-Content ../../artifacts/stage2_tests.txt
      $diff = Compare-Object (Get-Content ../../artifacts/before_tests.txt) (Get-Content ../../artifacts/stage2_tests.txt)
      if ($diff) { throw "Stage 2 test inventory mutated!`n$($diff | Out-String)" }
  } finally {
      Pop-Location
  }

  # 2.4.2 全量流水线验证 (CWD: 仓库根目录)
  powershell -NoProfile -ExecutionPolicy Bypass -File scripts\ci.ps1 test
  powershell -NoProfile -ExecutionPolicy Bypass -File scripts\finish-task.ps1
  ```

- [ ] **步骤 2.5：原子提交阶段二**
  ```powershell
  # (CWD: 仓库根目录)
  git add app/src-tauri/src/
  git commit -m "refactor(service): 收敛云同步测试为三层领域状态底座"
  ```
