# 已确认审计问题修复总览实现计划

> **面向 AI 代理的工作者：** 必需子技能：使用 superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans 逐任务实现此计划。步骤使用复选框（`- [ ]`）语法来跟踪进度。

**目标：** 依次修复审计中经代码核验成立的 16 项缺陷，并让每批修复都经过 TDD、定向验证、高强度审查和审查发现复核。

**架构：** 工作拆为 5 个可独立验证的批次，共享同一分支但不跨批混改。每批完成“红测 → 最小实现 → 定向回归 → 高强度审查 → 逐条核验 → 只修确认项 → 再审查 → 提交”后，才进入下一批；最后执行全仓门禁和一次跨批审查。

**技术栈：** Rust 1.97、Tokio、sqlx/SQLite、Tauri 2、Node.js 22、原生 userscript、Vue 3、TypeScript、Vitest 4、DOMPurify 3.4.14

---

## 文件结构

- `docs/superpowers/specs/2026-09-08-audit-confirmed-fixes-design.md`：已批准的范围、设计和完成标准；实现不得偏离。
- `docs/superpowers/plans/2026-09-08-audit-confirmed-fixes-semantic.md`：批次 1，#8/#9/#13/#15/#16。
- `docs/superpowers/plans/2026-09-08-audit-confirmed-fixes-persistence-path.md`：批次 2，#2/#18/#19/#24。
- `docs/superpowers/plans/2026-09-08-audit-confirmed-fixes-userscript-import.md`：批次 3，#25/#26/#27。
- `docs/superpowers/plans/2026-09-08-audit-confirmed-fixes-frontend.md`：批次 4，#28/#30/#31。
- `docs/superpowers/plans/2026-09-08-audit-confirmed-fixes-mcp-migration.md`：批次 5，#5。

## 执行约束

- 当前分支必须是 `fix/audit-confirmed-issues`；不得切到或直接推送 `main`。
- 不修改 #1、#3、#4、#6、#7、#10、#11、#12、#14、#17、#20、#21、#22、#23、#29、#32。
- 尤其不得放宽 `app/src-tauri/src/http_api.rs` 的写接口密钥校验。
- 每个红测必须在实现改动前运行并观察目标原因导致的失败；若测试意外通过，先修正测试，不得继续实现。
- 审查发现不是自动成立：必须回到触发输入、状态转换和可执行代码逐条验证，只修复确认项。
- 每个子计划中的提交命令是建议边界；执行时只暂存该任务列出的文件。

### 任务 1：建立实现基线

**文件：**
- 读取：`docs/superpowers/specs/2026-09-08-audit-confirmed-fixes-design.md`
- 读取：本总览列出的 5 个子计划

- [ ] **步骤 1：确认分支和干净基线**

运行：

```powershell
git branch --show-current
git status --short
```

预期：第一条输出 `fix/audit-confirmed-issues`；第二条只允许出现尚未提交的计划文档，不得出现实现文件改动。

- [ ] **步骤 2：记录工具链版本**

运行：

```powershell
rustc --version
cargo --version
node --version
npm --version
```

预期：命令均退出 0；Rust/Cargo 为 1.97.x，Node 为 22.x，npm 为 11.x。若版本不同，在执行记录中保留实际版本，但不要修改依赖来迎合本计划。

- [ ] **步骤 3：执行审计范围防漂移检查**

运行：

```powershell
@'
from pathlib import Path
import re
spec = Path('docs/superpowers/specs/2026-09-08-audit-confirmed-fixes-design.md').read_text(encoding='utf-8')
rows = re.findall(r'^\| [1-5] \| ([^|]+) \|', spec, re.M)
inside = {int(x) for row in rows for x in re.findall(r'#(\d+)', row)}
excluded_line = next(line for line in spec.splitlines() if line.startswith('本轮不修改 #'))
excluded = {int(x) for x in re.findall(r'#(\d+)', excluded_line)}
assert inside == {2, 5, 8, 9, 13, 15, 16, 18, 19, 24, 25, 26, 27, 28, 30, 31}
assert inside.isdisjoint(excluded)
assert inside | excluded == set(range(1, 33))
print('scope partition ok')
'@ | python -
```

预期：输出 `scope partition ok`。

### 任务 2：按顺序执行 5 个子计划

**文件：**
- 执行：`docs/superpowers/plans/2026-09-08-audit-confirmed-fixes-semantic.md`
- 执行：`docs/superpowers/plans/2026-09-08-audit-confirmed-fixes-persistence-path.md`
- 执行：`docs/superpowers/plans/2026-09-08-audit-confirmed-fixes-userscript-import.md`
- 执行：`docs/superpowers/plans/2026-09-08-audit-confirmed-fixes-frontend.md`
- 执行：`docs/superpowers/plans/2026-09-08-audit-confirmed-fixes-mcp-migration.md`

- [ ] **步骤 1：执行批次 1**

完整执行语义搜索子计划。预期：该计划的定向测试与批次回归全绿，高强度审查无确认遗留项，并产生其计划列出的任务提交。

- [ ] **步骤 2：执行批次 2**

仅在批次 1 闭环后执行持久化与路径子计划。预期：Windows 路径测试、设置测试和数据库连接测试全绿，审查无确认遗留项。

- [ ] **步骤 3：执行批次 3**

仅在批次 2 闭环后执行导入与 userscript 子计划。预期：normalizer 测试及 userscript 全套 Node 测试全绿，审查无确认遗留项。

- [ ] **步骤 4：执行批次 4**

仅在批次 3 闭环后执行前端子计划。预期：定向 Vitest、前端全套测试和生产构建全绿，审查无确认遗留项。

- [ ] **步骤 5：执行批次 5**

仅在批次 4 闭环后执行 MCP 迁移子计划。预期：service/MCP 定向集成测试全绿，审查无确认遗留项。

### 任务 3：最终全仓验证与跨批审查

**文件：**
- 检查：本轮全部实现与测试文件
- 不修改：审计明确排除项对应实现，除非编译所需的机械签名传播且行为不变

- [ ] **步骤 1：检查 userscript 语法与测试**

运行：

```powershell
node --check userscript/dist/ai-chat-memory.user.js
node --test userscript/tests/capture.test.mjs
```

预期：语法检查无输出并退出 0；测试摘要显示 0 failed。

- [ ] **步骤 2：检查前端构建与全套测试**

运行：

```powershell
npm --prefix app run build
npm --prefix app test
```

预期：Vue TypeScript 检查和 Vite 构建成功；Vitest 摘要显示所有测试通过。

- [ ] **步骤 3：检查 Rust 格式与静态分析**

运行：

```powershell
cargo fmt --manifest-path app/src-tauri/Cargo.toml --check
cargo clippy --manifest-path app/src-tauri/Cargo.toml --all-targets --all-features -- -D warnings
```

预期：两条命令退出 0，Clippy 无 warning。

- [ ] **步骤 4：运行 Rust 全套测试**

运行：

```powershell
cargo test --manifest-path app/src-tauri/Cargo.toml --all-features
```

预期：所有 test target 通过，0 failed。

- [ ] **步骤 5：执行跨批高强度审查**

审查范围：从设计提交 `0ae5538` 的父提交到当前 `HEAD` 的实现 diff，但排除纯计划文档。审查必须检查：取消票据跨后端传播、设置恢复失败原子性、userscript 队列持久化、导出 ready 屏障、MCP 所有读入口 guard、以及 16 项与 16 项排除范围是否漂移。

预期：输出按严重度排序的发现列表，或明确“无发现”；不得把格式偏好列为缺陷。

- [ ] **步骤 6：逐条验证最终审查发现**

对每条发现记录：触发输入、执行路径、错误结果、判定（CONFIRMED/REJECTED）及证据文件行。CONFIRMED 项先补红测再修复；REJECTED 项不改代码。修复后重跑步骤 1–4，并重复步骤 5，直到没有 CONFIRMED 遗留项。

- [ ] **步骤 7：核对最终 diff 与范围**

运行：

```powershell
git status --short
git diff --check 0ae5538..HEAD
git diff --stat 0ae5538..HEAD
```

预期：工作区干净；`git diff --check` 无输出；统计只包含 5 个子计划声明的实现、测试和依赖文件。

- [ ] **步骤 8：创建最终验证提交（仅当审查闭环产生了额外改动）**

```powershell
git add <逐条列出最终审查修复涉及的文件>
git commit -m "fix(audit): 收敛跨批审查确认问题`n`nCo-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

预期：若没有额外改动则跳过此提交；不得创建空提交。
