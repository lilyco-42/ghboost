#!/usr/bin/env python3
"""真模拟器上把「真跑流量」跑通一遍（Android 端到端闸）。

为什么需要它
------------
license-ui 闸能证明「APK 装得起、许可证点得开」；静态闸能证明「TUN 代码在包里」。
但「按 START 之后，这台设备上的流量到底有没有真的进代理内核、再出公网」、
「在 enforcing SELinux 下有没有被 avc 拒绝」这些，静态闸一句都答不了。
唯一可靠的做法：跑真模拟器、点真按钮、走真流量。跑的是 android-apk job 上传的
**产物本体**（不是重编件）。

要复现真实 VPN 使用链路，必须有「一个能连的节点」。公网免费节点会死会换，
不能拿来当闸的判据，所以本脚本在 runner 上起一个纯 stdlib 的 SOCKS5 relay
（127.0.0.1:24000），对模拟器来说它就是 10.0.2.2:24000 —— 一个**确定可连**的
本地节点（socks5://10.0.2.2:24000#ci-local），链路是：
    App(VpnService TUN) → tun2socks → 127.0.0.1:1080 → 内核 → 本地 relay → 公网
同时把 openproxylist 等订阅拉下来当额外节点（sanitize 后会保留），
公网节点活着就是真实 VPN 使用，死了也有本地节点兜底 —— 闸不赌公网心情。

断言分三档：
  1. 机械断言（硬）：btnStart 可点、VPN 授权弹窗点 OK、状态到 "VPN running"、
     tun0 接口在且有 10.0.0.2、adb forward 打内核 1080 端口出网 2xx/204。
  2. SELinux（硬）：getenforce == Enforcing，且干净窗口内 app 域的
     "avc: denied" 为零 —— 模拟器镜像就是 enforcing，这是能做到的最接近
     真机严格度的自动验证（各家 ROM 策略仍属真机专测，见计划文档）。
  3. TUN 包级探针（软）：模拟器镜像若有 nc/wget/curl，就用 shell UID
     （不在 disallowed 名单里，流量必须走 tun0）发一个原始 HTTP 请求，
     证明流量真的进了 TUN；没有工具就跳过并留档。
另加麦克风负向证明：App 清单里没有 RECORD_AUDIO，pm grant 必然失败。

用法：python3 android-e2e-test.py <x86_64-release.apk>
"""

import os
import re
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import android_license_ui_test as L  # 复用 adb/dump/wait_for/tap/describe/die/fail_with

PKG = L.PKG
ACTIVITY = L.ACTIVITY

# ── 确定性本机节点：host 上的 SOCKS5 relay ────────────────────────────────
LOCAL_SOCKS_PORT = 24000
LOCAL_NODE = "socks5://10.0.2.2:%d#ci-local" % LOCAL_SOCKS_PORT  # 模拟器 → host loopback
PROBE_URL = "https://connectivitycheck.gstatic.com/generate_204"
PLAIN_HOST = "neverssl.com"  # 走纯 HTTP 的常青目标，给 nc 探针用
FORWARD_PORT = 21080         # adb forward 主机端口 → 模拟器 1080（内核 SOCKS）
FORWARD_TARGET = "tcp:1080"

# ── 公网节点来源（订阅，非 README 索引）────────────────────────────────────
SUBSCRIPTIONS = [
    "https://raw.githubusercontent.com/roosterkid/openproxylist/refs/heads/main/V2RAY_RAW.txt",
    "https://raw.githubusercontent.com/pawdroid/Free-servers/main/sub",
]
MAX_PUBLIC_NODES = 30
SCHEME_RE = re.compile(r"^(ss|vmess|trojan|vless|socks5?|http|hysteria2?|tuic)://", re.I)

# App 的外部导入路径（importExternalNodes 的两个候选，见 LocalProxySetup.kt）
EXT_PATHS = [
    "/sdcard/Android/data/%s/files/ghboost-providers.yaml" % PKG,
    "/sdcard/Android/data/%s/files/ghboost/providers.yaml" % PKG,
]


def log(msg):
    L.log(msg)


def assert_ok(label, ok, detail=""):
    log(("  ok    " if ok else "  FAIL  ") + label + (("  —  " + detail) if detail else ""))
    if not ok:
        L.die(label + ((": " + detail) if detail else ""))


# ── host 侧 SOCKS5 relay（no-auth，仅 CONNECT，双向透明转发）──────────────
def _recv_exact(c, n):
    buf = b""
    while len(buf) < n:
        chunk = c.recv(n - len(buf))
        if not chunk:
            raise EOFError("connection closed early")
        buf += chunk
    return buf


def _pump(a, b):
    try:
        while True:
            data = a.recv(65536)
            if not data:
                break
            b.sendall(data)
    except OSError:
        pass
    finally:
        for s in (a, b):
            try:
                s.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass


def _handle(client):
    try:
        ver, nm = _recv_exact(client, 2)
        if ver != 5:
            return
        _recv_exact(client, nm)
        client.sendall(b"\x05\x00")                     # 只接受 no-auth
        _ver, cmd, _rsv, atyp = _recv_exact(client, 4)
        if cmd != 1:                                    # 只做 CONNECT
            client.sendall(b"\x05\x07\x00\x01\x00\x00\x00\x00\x00\x00")
            return
        if atyp == 1:
            host = socket.inet_ntoa(_recv_exact(client, 4))
        elif atyp == 3:
            ln = _recv_exact(client, 1)[0]
            host = _recv_exact(client, ln).decode("latin-1")
        elif atyp == 4:
            host = socket.inet_ntop(socket.AF_INET6, _recv_exact(client, 16))
        else:
            return
        port = int.from_bytes(_recv_exact(client, 2), "big")
        up = socket.create_connection((host, port), timeout=20)
        client.sendall(b"\x05\x00\x00\x01\x00\x00\x00\x00\x00\x00")
        t1 = threading.Thread(target=_pump, args=(client, up), daemon=True)
        t2 = threading.Thread(target=_pump, args=(up, client), daemon=True)
        t1.start()
        t2.start()
        t1.join()
        t2.join()
    except Exception:
        pass
    finally:
        try:
            client.close()
        except OSError:
            pass


class Socks5Server(threading.Thread):
    def __init__(self, port):
        super().__init__(daemon=True)
        self.srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.srv.bind(("127.0.0.1", port))
        self.srv.listen(32)

    def run(self):
        while True:
            try:
                c, _ = self.srv.accept()
            except OSError:
                return
            threading.Thread(target=_handle, args=(c,), daemon=True).start()


def curl_socks(port, url, timeout=20):
    """host 上走 SOCKS5 打一发；回传 (rc, http_code)。"""
    p = subprocess.run(
        ["curl", "-sS", "-m", str(timeout), "-o", "/dev/null", "-w", "%{http_code}",
         "--socks5-hostname", "127.0.0.1:%d" % port, url],
        capture_output=True, timeout=timeout + 10)
    return p.returncode, (p.stdout or b"").decode("utf-8", "replace").strip()


def fetch_text(url, tries=3, timeout=25):
    last = None
    for _ in range(tries):
        try:
            req = urllib.request.Request(url, headers={"User-Agent": "ghboost-ci-e2e"})
            with urllib.request.urlopen(req, timeout=timeout) as r:
                return r.read().decode("utf-8", "replace")
        except Exception as e:  # noqa: BLE001 —— 重试后统一报错
            last = e
            time.sleep(3)
    raise IOError("fetch %s failed after %d tries: %s" % (url, tries, last))


def build_provider():
    """订阅公网节点 + 本地 relay 节点；本地节点放第一个保底。"""
    lines = [LOCAL_NODE]
    seen = {LOCAL_NODE}
    for url in SUBSCRIPTIONS:
        try:
            text = fetch_text(url)
        except Exception as e:  # noqa: BLE001
            log("  (订阅 %s 拉取失败，跳过：%s)" % (url, e))
            continue
        text = text.replace("&amp;", "&")  # 订阅里常有 HTML 转义的 &
        for ln in text.splitlines():
            s = ln.strip()
            if SCHEME_RE.match(s) and s not in seen:
                seen.add(s)
                lines.append(s)
        if len(lines) >= 1 + MAX_PUBLIC_NODES:
            break
    log("  provider 候选：本地节点 1 + 公网节点 %d 条" % (len(lines) - 1))
    return "\n".join(lines) + "\n"


# ── 视图树小工具（复用 license 脚本的 dump 机制）─────────────────────────
def find_id(root, suffix):
    for n in L.nodes(root):
        if (n.get("resource-id") or "").endswith(suffix):
            return n
    return None


def find_text_exact(root, s):
    for n in L.nodes(root):
        if (n.get("text") or "").strip() == s:
            return n
    return None


def wait_text(s, tries=40, delay=1.5, what=""):
    node, root, raw = L.wait_for(lambda r: find_text_exact(r, s), tries=tries, delay=delay)
    if node is None:
        tail = ""
        p = L.adb("logcat", "-d", "-t", "300", check=False, timeout=120)
        txt = (p.stdout or b"").decode("utf-8", "replace")
        gh = [ln for ln in txt.splitlines()
              if re.search(r"GhBoost|mihomo|sing-box|xray|ffi|tun2socks|VpnService", ln, re.I)]
        if gh:
            tail = "\n  应用日志尾（最后 40 行）：\n" + "\n".join(gh[-40:])
        L.fail_with("等不到文本 %r%s%s"
                    % (s, ("（" + what + "）") if what else "", tail), root, raw)
    return node


def select_engine(label):
    spin, root, raw = L.wait_for(lambda r: find_id(r, "spinnerEngine"), tries=20, delay=1.5)
    if spin is None:
        L.fail_with("找不到引擎选择器 spinnerEngine", root, raw)
    L.tap(spin, "spinnerEngine")
    item, root, raw = L.wait_for(lambda r: find_text_exact(r, label), tries=10, delay=1.5)
    if item is None:
        L.fail_with("引擎下拉里没有 %r（应在 自動/內建/Mihomo/Xray/sing-box 中）" % label,
                    root, raw)
    L.tap(item, "engine option " + label)
    time.sleep(2)


# ── 各检查项 ─────────────────────────────────────────────────────────────
def tun_up():
    """tun0 接口在且有 10.0.0.2；回传 (ok, 输出) —— 不 die，给断言用。"""
    outs = []
    for cmd in (["shell", "ifconfig", "tun0"], ["shell", "ip", "addr", "show", "tun0"]):
        p = L.adb(*cmd, check=False, timeout=30)
        txt = (p.stdout or b"").decode("utf-8", "replace")
        outs.append(txt.strip())
        if "10.0.0.2" in txt:
            return True, txt.strip()
    return False, "\n".join(x for x in outs if x)


def probe_kernel_outbound():
    """硬：host → adb forward → 内核 1080 socks → 节点 → 公网，出网 2xx/204。"""
    L.adb("forward", "tcp:%d" % FORWARD_PORT, FORWARD_TARGET)
    for i in range(12):
        rc, code = curl_socks(FORWARD_PORT, PROBE_URL, timeout=15)
        if rc == 0 and code in ("200", "204"):
            return code
        log("  探针 #%d：rc=%s http=%r，重试" % (i + 1, rc, code))
        time.sleep(7)
    return None


def probe_tun_packet():
    """软：若镜像里有 nc/wget/curl，走 shell UID（不被 VPN 排除）发原始 HTTP，
    流量必须进 tun0 → tun2socks → 内核 → 出网，才是「真走了 TUN」。"""
    tool = None
    for cand in ("nc", "wget", "curl"):
        p = L.adb("shell", "command -v", cand, check=False)
        if p.returncode == 0 and p.stdout.strip():
            tool = cand
            break
    if tool is None:
        log("  模拟器镜像没有 nc/wget/curl —— TUN 包级探针跳过"
            "（保留 tun0 接口 + 路由断言 + 内核 outbound 断言）")
        return None
    log("  用 %s 做 TUN 包级探针（shell uid，不被 addDisallowedApplication 排除）" % tool)
    if tool == "nc":
        cmd = ('printf "GET / HTTP/1.0\\r\\nHost: %s\\r\\n\\r\\n" | nc -w 20 %s 80'
               % (PLAIN_HOST, PLAIN_HOST))
    elif tool == "wget":
        cmd = "wget -q -T 20 -O - http://%s/ || true" % PLAIN_HOST
    else:  # curl
        cmd = "curl -s -m 20 http://%s/" % PLAIN_HOST
    p = L.adb("shell", cmd, check=False, timeout=60)
    out = (p.stdout or b"").decode("utf-8", "replace")
    head = out.splitlines()[0][:120] if out.splitlines() else ""
    if "200" in out and any(k in out for k in ("HTTP", "<html", "<!DOCTYPE")):
        return "%s -> %r" % (tool, head)
    log("  %s 探针输出头：%r" % (tool, head[:160]))
    return None


def selinux_avc_scan():
    """硬：enforcing 且干净窗口内 app 域 avc: denied == 0。"""
    p = L.adb("shell", "getenforce", check=False)
    mode = (p.stdout or b"").decode("utf-8", "replace").strip()
    assert_ok("SELinux enforcing", mode == "Enforcing", "getenforce=%r" % mode)

    p = L.adb("logcat", "-d", check=False, timeout=120)
    txt = (p.stdout or b"").decode("utf-8", "replace")
    avcs = [ln for ln in txt.splitlines() if "avc: denied" in ln]
    ours = [ln for ln in avcs if "untrusted_app" in ln or "com.ghboost" in ln.lower()]
    log("  avc: denied 总数=%d，落在 app 域=%d" % (len(avcs), len(ours)))
    for ln in avcs[-8:]:
        log("    avc | %s" % ln[:200])
    assert_ok("app 域无 avc: denied", not ours,
              "%d 条：\n%s" % (len(ours), "\n".join(ours[:5])))


def mic_negative_proof():
    """硬：App 未声明 RECORD_AUDIO，pm grant 必然失败 → 证明 App 不碰麦克风。"""
    p = L.adb("shell", "pm", "grant", PKG, "android.permission.RECORD_AUDIO", check=False)
    out = ((p.stdout or b"") + (p.stderr or b"")).decode("utf-8", "replace").strip()
    assert_ok("App 无麦克风权限（pm grant RECORD_AUDIO 必须失败）",
              p.returncode != 0, "rc=%s %s" % (p.returncode, out[-120:]))


def main():
    if len(sys.argv) != 2:
        L.die("usage: %s <apk>" % sys.argv[0])
    apk = sys.argv[1]

    # ── host 侧确定性节点：SOCKS5 relay ──
    log("== host SOCKS5 relay (127.0.0.1:%d) ==" % LOCAL_SOCKS_PORT)
    server = Socks5Server(LOCAL_SOCKS_PORT)
    server.start()
    time.sleep(0.5)
    rc, code = curl_socks(LOCAL_SOCKS_PORT, PROBE_URL, timeout=20)
    assert_ok("relay 自检（host → relay → 公网）", rc == 0 and code in ("200", "204"),
              "rc=%s http=%r" % (rc, code))

    provider = build_provider()
    lines = provider.splitlines()
    log("== 订阅节点 ==")
    log("  provider 前两行：%r | %r"
        % (lines[0], lines[1][:60] if len(lines) > 1 else "<无公网节点>"))

    # ── 安装 + 首启（建目录/占位 provider）──
    log("== install ==")
    L.adb("install", "-r", "-t", apk, timeout=900)
    L.adb("shell", "pm", "grant", PKG, "android.permission.POST_NOTIFICATIONS", check=False)
    L.adb("shell", "input", "keyevent", "82", check=False)
    L.adb("shell", "wm", "dismiss-keyguard", check=False)
    L.adb("shell", "am", "force-stop", PKG, check=False)
    L.adb("shell", "am", "start", "-W", "-n", ACTIVITY, timeout=180)
    time.sleep(6)
    L.adb("shell", "am", "force-stop", PKG, check=False)

    # ── 注入节点：推到两个外部导入路径，重开 App 自动导入 ──
    log("== push 节点清单到外部导入路径 ==")
    tmp = os.path.join(tempfile.gettempdir(), "ghboost-e2e-provider.yaml")
    with open(tmp, "w", encoding="utf-8") as f:
        f.write(provider)
    L.adb("shell", "mkdir", "-p",
          "/sdcard/Android/data/%s/files/ghboost" % PKG, check=False)
    for path in EXT_PATHS:
        L.adb("push", tmp, path)
        log("  pushed %s" % path)

    # 干净窗口从此刻起：前面是安装/boot 噪音，之后的日志才算 app 行为
    L.adb("logcat", "-c", check=False)

    log("== 重开 App（自动导入外部节点）==")
    L.adb("shell", "am", "start", "-W", "-n", ACTIVITY, timeout=180)
    wait_text("Ready", tries=40, what="tunReady && 有真实节点 -> Ready")
    log("  ok  状态 Ready：tun2socks 转发就绪且有真实节点")

    # ── 选定内核：Mihomo（协议覆盖最全；选完持久化）──
    select_engine("Mihomo")

    # ── START：授权弹窗 → OK → 等内核起来 ──
    log("== START ==")
    btn, root, raw = L.wait_for(lambda r: find_id(r, "btnStart"), tries=20, delay=1.5)
    if btn is None:
        L.fail_with("找不到 Start 按钮", root, raw)
    if btn.get("enabled") != "true":
        L.fail_with("Start 按钮 disabled（enabled=%r）—— 节点没导入成功" % btn.get("enabled"),
                    root, raw)
    L.tap(btn, "btnStart")
    ok, root, raw = L.wait_for(lambda r: find_text_exact(r, "OK"), tries=8, delay=1.5)
    if ok is not None:
        log("  VPN 授权弹窗出现，点 OK")
        L.tap(ok, "VPN consent OK")
    else:
        log("  没看到 VPN 授权弹窗（可能已授权），继续等")
    wait_text("VPN running", tries=80, what="内核启动完成")
    log("  ok  状态 'VPN running'")
    time.sleep(3)  # 让 url-test 完成首次健康检查

    # ── tunnel 接口 ──
    log("== tun0 接口 ==")
    ok, detail = tun_up()
    assert_ok("tun0 up 且带 10.0.0.2", ok, "输出：\n" + detail)

    # ── 内核 outbound（硬）：host → adb forward → 1080 → 节点 → 公网 ──
    log("== 内核出网探针（adb forward → 内核 1080 → 节点 → 公网）==")
    code = probe_kernel_outbound()
    assert_ok("内核代理路径出网成功", code is not None, "http_code=%r" % code)

    # ── TUN 包级探针（软）──
    log("== TUN 包级探针（尽力而为）==")
    tun_result = probe_tun_packet()
    if tun_result is not None:
        log("  ok  TUN 包级出网成功：%s" % tun_result)
    else:
        log("  跳过/未遂：见上方原因（不判失败）")

    # ── SELinux ——
    log("== SELinux ==")
    selinux_avc_scan()

    # ── 麦克风负向证明 ──
    log("== 麦克风 ==")
    mic_negative_proof()

    # ── 结尾：仍活着、仍在 VPN running（没崩）──
    log("== 收尾确认 ==")
    wait_text("VPN running", tries=10, what="收尾时仍在前台运行")
    log("  ok  跑完一轮后 App 仍在前台且状态为 VPN running（无崩溃）")

    log("")
    log("PASS: 模拟器端到端 —— START 可用、VPN running、tun0 up、内核出口联动、"
        "SELinux enforcing 无 avc、App 无麦克风权限%s"
        % ("、TUN 包级出网成功" if tun_result else ""))


if __name__ == "__main__":
    main()