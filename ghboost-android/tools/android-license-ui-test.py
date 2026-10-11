#!/usr/bin/env python3
"""真模拟器上把 App 里的「授權聲明」点开一次。

为什么需要它
------------
build-all.yml 里那条静态闸（Verify the in-app license entry ...）能证明：接线在、
assets 在 APK 里、内容与暂存区逐字节相同。它**证明不了「使用者点得到、点开有字」**
——「打进包里」和「用户看得到」是两件事，而分发义务要的是后者。

本脚本补的就是后半截：它跑在真的启动了模拟器上，点 App 里那个按钮，读 App
真正渲染出来的视图树。跑的是 android-apk job 上传的**产物本体**（不是重编件）。

断言全部用 ASCII 子串，不依赖 CJK 匹配 —— 按钮认 resource-id，列表项认
"GPL-3.0"，正文认 GPL 首页那句；视图树的 text 与字体、渲染无关。

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


def dump():
    """uiautomator dump → 视图树根节点。文字取自视图树，与字体无关。"""
    last = ""
    for _ in range(8):
        adb("shell", "rm", "-f", "/sdcard/ui.xml", check=False)
        p = adb("shell", "uiautomator", "dump", "/sdcard/ui.xml", check=False)
        blob = (p.stdout + p.stderr).decode("utf-8", "replace")
        if "dumped to" in blob:
            xml = adb("exec-out", "cat", "/sdcard/ui.xml").stdout
            try:
                return ET.fromstring(xml)
            except ET.ParseError as e:
                last = "parse error: %s" % e
        else:
            last = blob.strip()[-300:]
        time.sleep(1.5)
    die("uiautomator dump 一直没成功：%s" % last)


def nodes(root):
    return list(root.iter("node"))


def find_id(root, suffix):
    for n in nodes(root):
        if (n.get("resource-id") or "").endswith(suffix):
            return n
    return None


def find_text(root, sub):
    for n in nodes(root):
        if sub in (n.get("text") or ""):
            return n
    return None


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


def all_text(root):
    return "\n".join((n.get("text") or "") for n in nodes(root))


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
    time.sleep(4)

    log("== step 1: 首页应能看到授權聲明按钮 ==")
    root = dump()
    btn = find_id(root, BTN_ID_SUFFIX) or find_text(root, "Licenses")
    if btn is None:
        die("找不到授權聲明按钮（resource-id 以 %s 结尾）。当前界面文字：\n%s"
            % (BTN_ID_SUFFIX, all_text(root)[:1500]))
    log("  ok  button present (text=%r)" % (btn.get("text") or ""))
    tap(btn, "button")

    log("== step 2: 应弹出清单，且含内核条目 ==")
    root = dump()
    txt = all_text(root)
    item = find_text(root, ITEM_TEXT)
    if item is None:
        die("清单里没有含 %r 的条目 —— 按钮点了，但没开出清单。界面文字：\n%s"
            % (ITEM_TEXT, txt[:1500]))
    log("  ok  list item present (text=%r)" % (item.get("text") or ""))
    tap(item, "list item")

    log("== step 3: 正文应是真 GPL 全文，而不是指针声明 ==")
    root = dump()
    txt = all_text(root)
    if BODY_TEXT not in txt:
        die("正文里没有 %r —— 按钮能点开，但内容不是真的 GPL 全文。界面文字：\n%s"
            % (BODY_TEXT, txt[:2000]))
    m = re.search(r"(\d+)\s*bytes", txt)
    if not m:
        die("正文里没看到 '<N> bytes' 大小行，拿不到「读了多少」的证据。\n%s" % txt[:1500])
    size = int(m.group(1))
    if size < MIN_LICENSE_BYTES:
        die("读到的正文只有 %d bytes（< %d）—— 像是指针声明或空文件，不是 GPL 全文。"
            % (size, MIN_LICENSE_BYTES))
    log("  ok  body has %r and reports %d bytes" % (BODY_TEXT, size))

    log("")
    log("PASS: 授權聲明按钮可点、清单可开、正文是真 GPL 全文（%d bytes）" % size)


if __name__ == "__main__":
    main()
