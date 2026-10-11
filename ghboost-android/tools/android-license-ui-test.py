#!/usr/bin/env python3
"""真模拟器上把 App 里的「授權聲明」点开一次。

为什么需要它
------------
build-all.yml 里那条静态闸（Verify the in-app license entry ...）能证明：接线在、
assets 在 APK 里、内容与暂存区逐字节相同。它**证明不了「使用者点得到、点开有字」**
——「打进包里」和「用户看得到」是两件事，而分发义务要的是后者。

本脚本补的就是后半截：它跑在真的启动了模拟器上，点 App 里那个按钮，读 App
真正渲染出来的视图树。跑的是 android-apk job 上传的**产物本体**（不是重编件）。

断言用 ASCII 子串，不依赖 CJK 匹配：列表项认 "GPL-3.0"，正文认 GPL 首页那句。
按钮则认 resource-id；认不到才退回文字（大小写不敏感 —— Button 默认
textAllCaps，布局里写的 "Licenses" 渲染出来是 "LICENSES"）。
视图树的 text/resource-id 与字体、渲染无关。

用法：python3 android-license-ui-test.py <x86_64-release.apk>
"""

import re
import subprocess
import sys
import time
import xml.etree.ElementTree as ET

PKG = "com.ghboost.app"
ACTIVITY = "com.ghboost.app/.MainActivity"
BTN_ID_SUFFIX = "btnLicenses"
# Button 默认 textAllCaps，所以布局里的 "Licenses" 会渲染成 "LICENSES" —— 比对照样大小写不敏感。
BTN_TEXTS = ("licenses", "授權聲明", "授权声明")
ITEM_TEXT = "GPL-3.0"
BODY_TEXT = "GNU GENERAL PUBLIC LICENSE"
# GPL 全文 ~35 KB、MPL 全文 ~16.7 KB；上游那种「详见 GPL-3.0」的指针声明只有
# 几百字节。用大小当判据，而不是「有字就算」—— 后者会放行指针声明。
MIN_LICENSE_BYTES = 10000


def die(msg):
    print("::error::" + " ".join(str(msg).split()))
    print(msg)
    sys.exit(1)


def log(msg):
    print(msg, flush=True)


def sh(*args, timeout=300, check=True):
    p = subprocess.run(args, capture_output=True, timeout=timeout)
    if check and p.returncode != 0:
        die("%s\n  rc=%s\n  stdout=%s\n  stderr=%s" % (
            " ".join(args), p.returncode,
            p.stdout.decode("utf-8", "replace")[-2000:],
            p.stderr.decode("utf-8", "replace")[-2000:]))
    return p


def adb(*args, **kw):
    return sh("adb", *args, **kw)


def dump_once():
    """回传 (root 或 None, raw 字串) —— 不 die，由 wait_for 统一重试。"""
    adb("shell", "rm", "-f", "/sdcard/ui.xml", check=False)
    p = adb("shell", "uiautomator", "dump", "/sdcard/ui.xml", check=False)
    blob = (p.stdout + p.stderr).decode("utf-8", "replace")
    if "dumped to" not in blob:
        return None, "uiautomator dump 没成功：\n%s" % blob.strip()[-400:]
    raw = adb("exec-out", "cat", "/sdcard/ui.xml").stdout.decode("utf-8", "replace")
    try:
        return ET.fromstring(raw), raw
    except ET.ParseError as e:
        return None, "XML 解析失败：%s\n%s" % (e, raw[:600])


def wait_for(pred, tries=12, delay=1.5):
    """轮询 dump 直到 pred(root) 回传真值；回传 (值或 None, 最后一次 root, 最后 raw)。"""
    last_root = last_raw = None
    for _ in range(tries):
        root, raw = dump_once()
        if root is not None:
            last_root, last_raw = root, raw
            v = pred(root)
            if v is not None:
                return v, root, raw
        else:
            last_raw = raw
        time.sleep(delay)
    return None, last_root, last_raw


def nodes(root):
    return list(root.iter("node"))


def all_text(root):
    return "\n".join((n.get("text") or "") for n in nodes(root))


def describe(root):
    """把视图树拍成一行一节点，用于失败时定位（resource-id 到底长什么样）。"""
    if root is None:
        return "  (没有可用的视图树)"
    rows = []
    for n in nodes(root):
        cls = (n.get("class") or "").split(".")[-1]
        rid = n.get("resource-id") or ""
        t = (n.get("text") or "").replace("\n", "\\n")[:56]
        rows.append("      %-14s id=%-44s text=%r" % (cls, rid, t))
    return "\n".join(rows)


def center(node):
    m = re.match(r"\[(\d+),(\d+)\]\[(\d+),(\d+)\]", node.get("bounds") or "")
    if not m:
        die("节点 bounds 解析不了：%r" % (node.get("bounds"),))
    x1, y1, x2, y2 = (int(v) for v in m.groups())
    return (x1 + x2) // 2, (y1 + y2) // 2


def tap(node, what):
    x, y = center(node)
    log("  tap %s at (%d,%d)" % (what, x, y))
    adb("shell", "input", "tap", str(x), str(y))
    time.sleep(1.5)


def find_button(root):
    for n in nodes(root):
        if (n.get("resource-id") or "").endswith(BTN_ID_SUFFIX):
            return n
    for n in nodes(root):
        tl = (n.get("text") or "").lower()
        if any(k.lower() in tl for k in BTN_TEXTS):
            return n
    return None


def fail_with(what, root, raw):
    die("%s\n  当前视图树：\n%s\n  原始 XML 前 800 字：\n%s"
        % (what, describe(root), (raw or "")[:800]))


def main():
    if len(sys.argv) != 2:
        die("usage: %s <apk>" % sys.argv[0])
    apk = sys.argv[1]

    log("== install ==")
    adb("install", "-r", "-t", apk, timeout=900)

    # 通知权限弹窗会盖住 UI：先授予，App 就不会问（API 33+ 才问）。
    adb("shell", "pm", "grant", PKG, "android.permission.POST_NOTIFICATIONS", check=False)

    log("== launch ==")
    adb("shell", "input", "keyevent", "82", check=False)          # 解开锁屏
    adb("shell", "wm", "dismiss-keyguard", check=False)
    adb("shell", "am", "force-stop", PKG, check=False)
    adb("shell", "am", "start", "-W", "-n", ACTIVITY, timeout=180)
    time.sleep(3)

    log("== step 1: 首页的授權聲明按钮 ==")
    btn, root, raw = wait_for(find_button)
    if btn is None:
        fail_with("找不到授權聲明按钮（resource-id 以 %s 结尾，或文字含 %s）"
                  % (BTN_ID_SUFFIX, BTN_TEXTS), root, raw)
    log("  ok  button present (id=%r text=%r)"
        % (btn.get("resource-id") or "", btn.get("text") or ""))
    tap(btn, "button")

    log("== step 2: 应弹出清单，且含内核条目 ==")
    item, root, raw = wait_for(lambda r: find_text_node(r, ITEM_TEXT))
    if item is None:
        fail_with("清单里没有含 %r 的条目 —— 按钮点了，但没开出清单（或对话框不在视图树里）"
                  % ITEM_TEXT, root, raw)
    log("  ok  list item present (text=%r)" % (item.get("text") or ""))
    tap(item, "list item")

    log("== step 3: 正文应是真 GPL 全文，而不是指针声明 ==")
    body, root, raw = wait_for(lambda r: body_ok(r))
    if body is None:
        fail_with("正文没有 %r，或没有 >= %d bytes 的大小行 —— 按钮能点开，"
                  "但内容不是真的 GPL 全文" % (BODY_TEXT, MIN_LICENSE_BYTES), root, raw)
    size = body
    log("  ok  body has %r and reports %d bytes" % (BODY_TEXT, size))

    log("")
    log("PASS: 授權聲明按钮可点、清单可开、正文是真 GPL 全文（%d bytes）" % size)


def find_text_node(root, sub):
    for n in nodes(root):
        if sub in (n.get("text") or ""):
            return n
    return None


def body_ok(root):
    """正文到位就回传读到的字节数（int），否则 None。"""
    txt = all_text(root)
    if BODY_TEXT not in txt:
        return None
    m = re.search(r"(\d+)\s*bytes", txt)
    if not m:
        return None
    size = int(m.group(1))
    return size if size >= MIN_LICENSE_BYTES else None


if __name__ == "__main__":
    main()
