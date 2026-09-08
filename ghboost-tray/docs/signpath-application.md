# SignPath Foundation 免费代码签名 —— 申请与接入

> 面向 ghboost 的 Windows 发行（ghboost-tray）。
> 背景与选型依据见 `../README.md` 第六节「代码签名」。

## 为什么是它

没签名的 exe 在 Windows 上会被 SmartScreen 拦成「Windows 已保护你的电脑」，
小白看到就直接关掉。2026 年的可选路径里：

| 方案 | 结论 |
|---|---|
| EV 证书 | **排除**。2024 年起不再有即时信誉特权，与 OV 同流程，$400+/年的溢价没有意义 |
| Azure Artifact Signing（$9.99/月） | **排除**。地域限制：组织限美/加/欧盟/英国，**个人开发者仅限美、加** —— 台湾身份用不了 |
| **SignPath Foundation** | **采用**。给 OSS 的免费 OV 级证书，**无地域限制** |
| 付费 OV（$150–300/年） | 后备。想让发行者显示自有品牌名时再切 |

代价要说清楚：

- 证书发行者是 **SignPath Foundation**，不是我们；Windows 里显示的发行者名是它
- 构建必须**全自动化且可追溯到仓库源码** —— 人工本地编完再上传的不给签
  （这是 `.github/workflows/tray.yml` 存在的理由）
- 违反其[行为准则](https://signpath.org/terms)可**即时或追溯吊销**证书

## 申请前必做（缺一项就可能被拒）

1. **所有维护者开 MFA**：GitHub 账号 + SignPath 账号都要
2. **明确 Authors / Reviewers 角色分工**：外部贡献的 PR 必须经 Reviewer 审阅。
   建议在 main 的分支保护上补：
   - Require a pull request before merging
   - Required approvals: 1
3. **已有 release**：✅ v0.3.0 已发布，含 `ghboost-tray-windows-x64.zip`
4. **下载页写明功能**：✅ 仓库 README + Release 说明
5. **提供卸载方式**：✅ `tools/uninstall.ps1` + `uninstall.bat`
   （这是行为准则里的**硬性要求**，不是加分项）

## 申请表单（signpath.org → Apply for Free Code Signing）

可直接复制：

```
Project name:        ghboost
Repository URL:      https://github.com/lilyco-42/ghboost
Download page URL:   https://github.com/lilyco-42/ghboost/releases
License:             MIT (OSI-approved)
Build system:        GitHub Actions (.github/workflows/tray.yml)

Description:
ghboost accelerates access to GitHub / Google / YouTube by measuring
candidate IPs and writing the fastest ones into the local hosts file,
and can import a proxy subscription into a bundled mihomo kernel.
ghboost-tray is the Windows desktop shell: a tray icon with a status
light plus a single big button; the settings panel opens in the user's
default browser and talks to a loopback-only HTTP console.
It never uploads user data; the subscription URL is used locally only.
Writing the hosts file requires administrator rights, and the UI says so
explicitly.
```

审核通常几天到几周。批准后拿到：Organization ID、Project Slug、Signing Policy Slug。

## 批准后接入

1. 安装 GitHub App：<https://github.com/apps/signpath>，只对 ghboost 仓库授权
2. SignPath Dashboard → **Trusted Build Systems** → 添加 `github.com`，绑定本仓库
3. **Artifact Configurations**：GitHub 打包的产物是 zip，根元素要设成 ZIP，
   并在其中对 PE 文件（exe）签名
4. **My Profile → API Tokens**：建一个 Submitter 权限的 token（只显示一次）
5. 仓库 Settings → Secrets 加四个：
   `SIGNPATH_API_TOKEN` / `SIGNPATH_ORGANIZATION_ID` /
   `SIGNPATH_PROJECT_SLUG` / `SIGNPATH_SIGNING_POLICY_SLUG`
6. 在 `tray.yml` 打包之后插入签名步骤（草案）：

```yaml
      - name: Upload unsigned artifact
        id: upload
        uses: actions/upload-artifact@v4
        with:
          name: ghboost-tray-windows-x64
          path: ghboost-tray-windows-x64.zip

      - name: Sign with SignPath
        uses: signpath/github-action-submit-signing-request@v1
        with:
          api-token: ${{ secrets.SIGNPATH_API_TOKEN }}
          organization-id: ${{ secrets.SIGNPATH_ORGANIZATION_ID }}
          project-slug: ${{ secrets.SIGNPATH_PROJECT_SLUG }}
          signing-policy-slug: ${{ secrets.SIGNPATH_SIGNING_POLICY_SLUG }}
          artifact-configuration-slug: zip
          github-artifact-id: ${{ steps.upload.outputs.artifact-id }}
          wait-for-completion: true
          output-artifact-directory: signed
```

注意：`upload-artifact` 必须带 `id: upload` 才能拿到 `artifact-id`；
签名后的产物要用**签名版本**覆盖原产物再挂 Release，否则 Release 上还是未签名的。

## 三个容易踩的坑

1. **签名 ≠ 立刻没警告**。它只是让信誉开始累积；最初几个版本仍会警告。
   所以下载页要直接教用户「详细信息 → 仍要执行」，并给 SHA256 校验和。
2. **发行者身份必须稳定**。换证书 / 改发行者名 = 信誉从零重来。
3. **别签上游二进制**。mihomo（GPL-3.0）是独立子进程，包里带未签名的
   上游二进制是允许的，但不要试图替它签名 —— 那是上游项目自己的事。
