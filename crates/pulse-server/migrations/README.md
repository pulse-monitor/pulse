# 数据库迁移

## ⚠️ 已发布的迁移文件一个字节都不能再改

sqlx 会对每个迁移文件算校验和并记在数据库里。文件内容变了（**哪怕只改注释**），
启动时就会失败：

```
Error: 打开数据库失败: sqlite://…/pulse.db
Caused by:
    migration 1 was previously applied but has been modified
```

已部署的实例会全部起不来，而且从错误信息看不出「是谁改了它」。

**这个坑真踩过**：一次目录重构里，批量替换文档路径的脚本顺手把迁移文件注释里的
`docs/02-data-model.md` 改成了 `docs/design/02-data-model.md`，
真机上的面板当场进入重启循环。

所以：

- 迁移文件里的文档链接**即使目录变了也保持原样**，新链接写到新迁移或代码注释里
- 任何批量替换、格式化工具都要把 `migrations/` 排除在外
- 要改 schema 就**加一个新文件**，永远不要动旧的

## 方言

`sqlite/` 与 `postgres/` 两套分开维护 —— 两边的类型系统和索引语法差别大到
不值得用一套 SQL 硬凑（见 `docs/design/02-data-model.md`）。
