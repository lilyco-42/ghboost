/*
 * ghboost.h — ghboost C ABI (foreign function interface)
 * ============================================================================
 *
 * ghboost 把两大子系统的核心能力打包成 C ABI 动态库，供其它语言 / 设备 /
 * 平台直接嵌入调用：
 *   - GitHub hosts 优选（boost）
 *   - 免费代理节点 扫描 / 测速 / 注入（scan / test / add）
 *
 * 编译产物（随平台命名不同）：
 *   Linux   : libghboost.so
 *   macOS   : libghboost.dylib
 *   Windows : ghboost.dll
 *
 * 调用约定（务必遵守）：
 *   1. 所有函数入参都是 **JSON 字符串**（`const char *`，UTF-8）。
 *      - 传 NULL / 空串 / "{}" 都表示使用全部默认参数。
 *      - 字段缺失同样取默认值；仅填需要的字段即可。
 *   2. 所有函数出参都是 **堆上分配的 JSON 字符串**（`char *`，UTF-8）。
 *      - 成功：`{"...": ...}`（结构见各函数说明）。
 *      - 失败：`{"error": "人类可读原因"}`。
 *   3. **调用方必须用 `ghboost_free()` 释放出参指针**，否则内存泄漏。
 *      - 传 NULL 给 `ghboost_free()` 是安全的（空操作）。
 *   4. 函数是线程安全的（内部各自走独立的 tokio 运行时），但返回的
 *      字符串指针仅在被 `ghboost_free` 之前有效，勿跨线程共享同一指针。
 *
 * JSON 字段与 CLI 默认值一致，完整 schema 见仓库 README 的「C ABI」一节。
 * ============================================================================
 */

#ifndef GHBOOST_H
#define GHBOOST_H

#ifdef __cplusplus
extern "C" {
#endif

/* ---------------------------------------------------------------------------
 * 内存管理
 * ------------------------------------------------------------------------- */

/**
 * 释放本库任意 `ghboost_*` 函数返回的字符串指针。
 *
 * @param ptr 本库返回、且尚未释放的指针；传 NULL 安全（无操作）。
 */
void ghboost_free(char *ptr);

/* ---------------------------------------------------------------------------
 * GitHub hosts 优选
 * ------------------------------------------------------------------------- */

/**
 * GitHub hosts 优选（等价于 CLI 的 `ghboost` / `--apply` / `--clean`）。
 *
 * 入参 JSON 字段（全部可选，缺省取默认值）：
 *   timeout_ms : number  单 IP 测速超时毫秒（默认 3000，范围 200–15000）
 *   concurrency: number  并发测速数（默认 16）
 *   top        : number  每域名保留的最优 IP 数（默认 1）
 *   extra_ip   : string[] 额外候选 IP 列表
 *   only       : string[] 只处理指定域名（默认全部 GitHub 域名）
 *   apply      : boolean  true=写入系统 hosts（需管理员/root 权限）
 *   clean      : boolean  true=清理 ghboost 已写入的 hosts 条目
 *
 * 出参 JSON 示例：
 *   {
 *     "applied": false,
 *     "rows": [ {"domain":"github.com","best_ip":"...","best_ms":12, ...} ],
 *     ...
 *   }
 */
char *ghboost_boost(const char *params_json);

/* ---------------------------------------------------------------------------
 * 节点扫描 / 测速 / 注入
 * ------------------------------------------------------------------------- */

/**
 * 节点扫描（等价于 `ghboost scan`）。
 *
 * 入参 JSON 字段（全部可选）：
 *   source      : string[] 追加的自定义订阅源 URL
 *   include_repo: boolean  是否扫描内置 free-VPN 仓库 README（默认 true）
 *   max_sources : number   最多抓取的源数（默认 60）
 *   concurrency : number   并发抓取数（默认 16）
 *   per_limit   : number   每源最多保留节点数（默认 500）
 *   output      : string   节点库存目录（默认 "./nodes_data"）
 *
 * 出参 JSON 示例：{ "sources": 3, "nodes": 127, ... }
 */
char *ghboost_scan(const char *params_json);

/**
 * 节点测速（等价于 `ghboost test`，需本机 Mihomo 内核）。
 *
 * 入参 JSON 字段（全部可选）：
 *   input     : string  扫描结果目录（默认 "./nodes_data"）
 *   top       : number  最多测速节点数（默认 300）
 *   concurrency: number 并发测速数（默认 32）
 *   timeout_ms: number  单次测速超时毫秒（默认 8000）
 *   test_url  : string  测速探测地址（默认 "https://www.gstatic.com/generate_204"）
 *   mihomo    : string  可选，Mihomo 可执行文件路径（显式指定时优先级最高）。
 *              缺省时按下述顺序自动探测：环境变量 GHBOOST_MIHOMO → 与可执行文件
 *              同目录的内置内核（Release 自带的 mihomo-<triple>）→ 系统标准路径
 *              （/usr/local/bin、/opt/homebrew/bin、Clash Verge 安装目录）→ PATH 里的 mihomo。
 *
 * 出参 JSON 示例：{ "tested": 120, "results": [ {"node": {...}, "delay_ms": 88}, ... ] }
 */
char *ghboost_test(const char *params_json);

/**
 * 节点导出 / 注入（等价于 `ghboost add`）。
 *
 * 入参 JSON 字段（全部可选）：
 *   input  : string  测速结果目录（默认 "./nodes_data"）
 *   keep   : number  导出延迟最低的节点数（默认 20）
 *   max_ms : number  延迟上限，超过则丢弃（默认 0=不限制，单位毫秒）
 *   apply  : boolean true=注入当前激活的 Clash/Mihomo profile（带备份）
 *   profile: string  可选，目标 profile 文件路径（默认自动探测）
 *
 * 出参 JSON 示例：{ "kept": 20, "exported_to": "...", ... }
 */
char *ghboost_add(const char *params_json);

/* ---------------------------------------------------------------------------
 * 元信息
 * ------------------------------------------------------------------------- */

/**
 * 版本信息。出参 JSON：`{"name":"ghboost","version":"x.y.z"}`。
 * 返回的指针同样必须用 `ghboost_free()` 释放。
 */
char *ghboost_version(void);

#ifdef __cplusplus
}
#endif

#endif /* GHBOOST_H */
