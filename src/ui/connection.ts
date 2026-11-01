import { api, type Config, type NetworkStatus, type WriteProbe } from "../api";
import { t } from "../i18n";
import { el, setStatus } from "./dom";
import { errorText, formatBytes, escapeHtml } from "./format";
import { toast } from "./toast";
import { localLog } from "./logs";
import { busy, requireConfig } from "./state";
import { refreshSecretStatus } from "./secret";
import { renderConfig } from "./settings";
import { proxyDraft, proxyValidationError } from "./proxy";

// 连接页：WebDAV 连通性、写探测、远端列表与网络状态。

export function connectionDraft(): Config {
  const base = requireConfig();
  return {
    ...base,
    webdav: {
      url: el.url.value.trim(),
      username: el.user.value,
      password: el.pass.value,
      // 留空 = 服务器根（SPEC-3 §A4）。
      remote_dir: el.dir.value.trim(),
    },
    proxy: proxyDraft(),
  };
}

export async function saveConnection(notify = true): Promise<void> {
  try {
    const draft = connectionDraft();
    // 保存时校验代理地址：非法 URL 不落盘（后端也会再校验一次）。
    const proxyError = proxyValidationError(draft.proxy);
    if (proxyError) {
      throw new Error(t("proxy.invalid", { error: proxyError }));
    }
    const saved = await api.setConfig(draft);
    renderConfig(saved);
    if (notify) {
      toast(t("conn.saved"));
    }
    void refreshSecretStatus();
  } catch (error) {
    setStatus(el.connStatus, "err", t("conn.saveFail", { error: errorText(error) }));
    throw error;
  }
}

export async function testConnection(notify = true): Promise<WriteProbe | null> {
  el.test.disabled = true;
  setStatus(el.connStatus, "idle", t("conn.testing"));
  try {
    await saveConnection(false);
    const result = await api.testConnection();
    const lines = [
      result.ok ? t("conn.testOk") : t("conn.testFail"),
      t("conn.latency", { ms: result.latency_ms }),
      t("conn.server", { name: result.server_type }),
      t("conn.dav", { value: result.dav_class || t("conn.davNone") }),
      result.remote_dir_missing
        ? t("conn.dirWillCreate")
        : result.remote_dir_exists
          ? t("conn.dirExists")
          : t("conn.dirMissing"),
    ];
    if (result.message) {
      lines.push(result.message);
    }
    if (result.hint) {
      lines.push(result.hint);
    }
    const probe = await api.probeWrite();
    lines.push(
      probe.ok
        ? t("conn.probeOk")
        : t("conn.probeFail") + " · " + t("conn.probeStep", { step: probe.step, status: probe.http_status }),
    );
    if (!probe.ok && probe.hint) {
      lines.push(probe.hint);
    }
    setStatus(el.connStatus, result.ok && probe.ok ? "ok" : "err", lines.join(" · "));
    localLog("test: " + lines.join(" · "));
    if (!probe.ok && probe.hint) {
      toast(probe.hint);
    } else if (notify) {
      toast(result.ok ? t("conn.testOk") : t("conn.testFail"));
    }
    return probe;
  } catch (error) {
    const text = errorText(error);
    setStatus(el.connStatus, "err", t("conn.testFail") + "：" + text);
    localLog(t("conn.testFail") + "：" + text);
    toast(t("conn.testFail") + "：" + text);
    return null;
  } finally {
    el.test.disabled = busy;
  }
}

export async function probeWrite(): Promise<void> {
  el.probe.disabled = true;
  setStatus(el.connStatus, "idle", t("conn.probeTesting"));
  try {
    const probe = await api.probeWrite();
    const lines = [
      probe.ok ? t("conn.probeOk") : t("conn.probeFail"),
      t("conn.probeStep", { step: probe.step, status: probe.http_status }),
    ];
    if (probe.hint) {
      lines.push(probe.hint);
    }
    setStatus(el.connStatus, probe.ok ? "ok" : "err", lines.join(" · "));
    localLog("probe: " + lines.join(" · "));
    if (probe.hint) {
      toast(probe.hint);
    }
  } catch (error) {
    setStatus(el.connStatus, "err", t("conn.probeFail") + "：" + errorText(error));
  } finally {
    el.probe.disabled = busy;
  }
}

export async function listRemote(): Promise<void> {
  el.listRemote.disabled = true;
  try {
    const remote = await api.listRemote();
    const parts: string[] = [];
    parts.push("<h3>" + escapeHtml(t("conn.remoteDevices", { count: remote.devices.length })) + "</h3>");
    if (remote.devices.length === 0) {
      parts.push('<p class="muted">' + escapeHtml(t("conn.remoteDevicesEmpty")) + "</p>");
    } else {
      parts.push(
        "<ul>" +
          remote.devices
            .map((device) => "<li>" + escapeHtml(device.id) + " · " + device.files + "</li>")
            .join("") +
          "</ul>",
      );
    }
    parts.push("<h3>" + escapeHtml(t("conn.remoteShared", { count: remote.shared.length })) + "</h3>");
    if (remote.shared.length === 0) {
      parts.push('<p class="muted">' + escapeHtml(t("conn.remoteSharedEmpty")) + "</p>");
    } else {
      parts.push(
        "<ul>" +
          remote.shared
            .slice(0, 30)
            .map(
              (file) =>
                "<li>" +
                escapeHtml(file.path) +
                (file.size === null ? "" : " · " + formatBytes(file.size)) +
                "</li>",
            )
            .join("") +
          "</ul>",
      );
    }
    el.remoteList.innerHTML = parts.join("");
    el.remoteList.hidden = false;
  } catch (error) {
    setStatus(el.connStatus, "err", t("conn.remoteFail", { error: errorText(error) }));
  } finally {
    el.listRemote.disabled = busy;
  }
}

/* ---------- 网络状态（SPEC-3 §C10） ---------- */

export function formatRetry(seconds: number): string {
  if (seconds <= 0) {
    return t("network.retryDue");
  }
  if (seconds < 60) {
    return t("network.retrySoon");
  }
  const minutes = Math.ceil(seconds / 60);
  return t("network.retryIn", { minutes });
}

export async function refreshNetwork(): Promise<void> {
  try {
    const status: NetworkStatus = await api.networkStatus();
    const lines = [t("network.label", { state: status.message })];
    if (status.state === "metered") {
      lines.push(t("network.meteredNote"));
    }
    if (typeof status.next_retry_in_secs === "number") {
      lines.push(formatRetry(status.next_retry_in_secs));
    }
    el.networkStatus.textContent = lines.join(" · ");
    el.networkStatus.className =
      "hint network-" + status.state + (status.state === "online" ? "" : " warn");
  } catch {
    el.networkStatus.textContent = "";
  }
}
