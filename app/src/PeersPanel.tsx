import { useEffect, useRef, useState } from "react";
import * as api from "./api";
import type { PeerInfo, TrustedDevice } from "./types";

// Background sync (the Android foreground service) only exists on Android;
// hide its toggle everywhere else. Tauri's Android webview is Chromium, so
// its user agent carries "Android"; desktop webviews never do.
const IS_ANDROID = /android/i.test(navigator.userAgent);

function shortId(id: string): string {
  if (id.length <= 16) return id;
  return `${id.slice(0, 8)}…${id.slice(-4)}`;
}

async function copyText(s: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(s);
  } catch {
    // clipboard may be unavailable; ignore
  }
}

function IdChip({ id }: { id: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <button
      className="id-chip"
      title={`${id}\n(tap to copy)`}
      onClick={() => {
        void copyText(id).then(() => {
          setCopied(true);
          window.setTimeout(() => setCopied(false), 1200);
        });
      }}
    >
      {copied ? "copied" : shortId(id)}
    </button>
  );
}

export function PeersPanel(props: {
  deviceId: string;
  deviceName: string;
  peers: PeerInfo[];
  busy: boolean;
  onSync: (peer: PeerInfo) => void;
  onSyncAll: () => void;
  autosaveSecs: number;
  onAutosaveSecs: (n: number) => void;
  buildSha: string;
  vaultPath: string;
  onLog?: (m: string) => void;
  log: string[];
  theme: "dark" | "light";
  onTheme: (t: "dark" | "light") => void;
  onClose?: () => void;
}) {
  const logRef = useRef<HTMLDivElement>(null);
  const [trusted, setTrusted] = useState<TrustedDevice[]>([]);
  const [autosyncSecs, setAutosyncSecs] = useState(30);
  const [autosaveDraft, setAutosaveDraft] = useState(props.autosaveSecs);
  const [p2pPort, setP2pPort] = useState(8788);
  const [p2pPortLive, setP2pPortLive] = useState(0);
  const [p2pRunning, setP2pRunning] = useState(false);
  const [busyRestart, setBusyRestart] = useState(false);
  const [bgSync, setBgSync] = useState(
    localStorage.getItem("sn.bgSync") !== "0"
  );
  // In-app confirmation (window.confirm in the webview is unreliable: the
  // native dialog can close without the result reaching the JS).
  const [confirmPeer, setConfirmPeer] = useState<PeerInfo | null>(null);

  useEffect(() => {
    let cancelled = false;
    api
      .listTrusted()
      .then((t) => {
        if (!cancelled) setTrusted(t);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    let cancelled = false;
    api
      .p2pSettings()
      .then((s) => {
        if (cancelled) return;
        setAutosyncSecs(s.autosync_secs);
        setP2pPort(s.p2p_port_setting);
        setP2pPortLive(s.p2p_port);
        setP2pRunning(s.p2p_running);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    if (logRef.current) logRef.current.scrollTop = logRef.current.scrollHeight;
  }, [props.log]);

  const applyAutosync = () => {
    const n = Math.max(1, Math.min(3600, Math.round(autosyncSecs) || 30));
    setAutosyncSecs(n);
    localStorage.setItem("sn.autosyncSecs", String(n));
    api
      .setAutosyncSecs(n)
      .then(() => props.onLog?.(`sync interval: ${n}s`))
      .catch((e) => props.onLog?.(`sync interval: ${e}`));
  };

  const applyAutosave = () => {
    const n = Math.max(1, Math.min(300, Math.round(autosaveDraft) || 2));
    setAutosaveDraft(n);
    localStorage.setItem("sn.autosaveSecs", String(n));
    props.onAutosaveSecs(n);
    props.onLog?.(`autosave: ${n}s`);
  };

  const applyPort = () => {
    const n = Math.max(1, Math.min(65535, Math.round(p2pPort) || 8788));
    setP2pPort(n);
    localStorage.setItem("sn.p2pPort", String(n));
    api
      .setP2pPortSetting(n)
      .then(() => props.onLog?.(`P2P port: ${n} (applies after restart)`))
      .catch((e) => props.onLog?.(`P2P port: ${e}`));
  };

  const doRestart = async () => {
    setBusyRestart(true);
    try {
      const port = await api.restartP2p();
      setP2pPortLive(port);
      setP2pRunning(true);
      props.onLog?.(`P2P restarted on port ${port}`);
    } catch (e) {
      props.onLog?.(`P2P restart: ${e}`);
    } finally {
      setBusyRestart(false);
    }
  };

  const isTrusted = (id: string) => trusted.some((t) => t.device_id === id);

  const askTrust = (peer: PeerInfo) => {
    if (isTrusted(peer.device_id)) {
      api
        .untrustDevice(peer.device_id)
        .then((t) => {
          setTrusted(t);
          props.onLog?.(`trust removed: "${peer.name}"`);
        })
        .catch((e) => props.onLog?.(`trust: error — ${e}`));
    } else {
      setConfirmPeer(peer);
    }
  };

  const confirmTrust = () => {
    const peer = confirmPeer;
    if (!peer) return;
    setConfirmPeer(null);
    api
      .trustDevice(peer.device_id, peer.name)
      .then((t) => {
        setTrusted(t);
        props.onLog?.(`trusted device: "${peer.name}"`);
      })
      .catch((e) => props.onLog?.(`trust: error — ${e}`));
  };

  return (
    <aside className="panel">
      {props.onClose && (
        <button className="close-panel" title="Close" onClick={props.onClose}>
          ✕
        </button>
      )}
      <h2>My device</h2>
      <div className="pad selfdev">
        <div className="peer-name">{props.deviceName}</div>
        <div className="muted small" title={props.deviceId}>
          <IdChip id={props.deviceId} />
        </div>
        <div className="muted small">
          Sync works with trusted devices only
        </div>
      </div>

      <h2>Network</h2>
      <div className="pad">
        <button
          className="wide"
          disabled={props.busy || !props.peers.length}
          onClick={props.onSyncAll}
        >
          Sync with all trusted
        </button>
      </div>
      <div className="pad peers">
        {props.peers.length === 0 && (
          <div className="muted">No peers found (mDNS)</div>
        )}
        {props.peers.length > 0 && (
          <div className="muted small">
            Sync works after "Trust" on both devices (the ID to compare is
            shown in the confirm box)
          </div>
        )}
        {props.peers.map((p) => {
          const trusted = isTrusted(p.device_id);
          const confirming = confirmPeer?.device_id === p.device_id;
          return (
            <div key={p.device_id} className="peer">
              <div className="peer-meta">
                <div className="peer-name">
                  {p.name} {trusted && <span className="trust-badge">✓</span>}
                </div>
                <div className="muted small">
                  {p.address}:{p.port}
                </div>
                <div className="muted small">
                  <IdChip id={p.device_id} />
                </div>
              </div>
              <div className="peer-actions">
                <button
                  title={
                    trusted
                      ? "Remove trust"
                      : "Trust: sync becomes bidirectional — compare the IDs on both devices"
                  }
                  className={trusted ? "trust on" : "trust"}
                  onClick={() => askTrust(p)}
                >
                  {trusted ? "Trusted ✓" : "Trust"}
                </button>
                <button
                  disabled={props.busy || !trusted}
                  title={trusted ? "Sync" : "Trust the device first"}
                  onClick={() => props.onSync(p)}
                >
                  Sync
                </button>
              </div>
              {confirming && (
                <div className="trust-confirm">
                  <div className="muted small">
                    Trust "{p.name}"? Compare the ID with the one shown on
                    that device:
                  </div>
                  <div className="muted small trust-id">{p.device_id}</div>
                  <div className="peer-actions">
                    <button className="trust on" onClick={confirmTrust}>
                      Confirm
                    </button>
                    <button onClick={() => setConfirmPeer(null)}>
                      Cancel
                    </button>
                  </div>
                </div>
              )}
            </div>
          );
        })}
      </div>

      <h2>Advanced</h2>
      <div className="pad adv">
        <label className="adv-row">
          <span>Theme</span>
          <select
            className="inp"
            value={props.theme}
            onChange={(e) => props.onTheme(e.target.value as "dark" | "light")}
          >
            <option value="dark">Dark</option>
            <option value="light">Light</option>
          </select>
        </label>
        <label className="adv-row">
          <span>Sync interval, s</span>
          <input
            className="inp num"
            type="number"
            min={1}
            max={3600}
            value={autosyncSecs}
            onChange={(e) => setAutosyncSecs(Number(e.target.value))}
            onBlur={applyAutosync}
            onKeyDown={(e) => e.key === "Enter" && applyAutosync()}
          />
        </label>
        <label className="adv-row">
          <span>Autosave, s</span>
          <input
            className="inp num"
            type="number"
            min={1}
            max={300}
            value={autosaveDraft}
            onChange={(e) => setAutosaveDraft(Number(e.target.value))}
            onBlur={applyAutosave}
            onKeyDown={(e) => e.key === "Enter" && applyAutosave()}
          />
        </label>
        <label className="adv-row">
          <span>P2P port</span>
          <input
            className="inp num"
            type="number"
            min={1}
            max={65535}
            value={p2pPort}
            onChange={(e) => setP2pPort(Number(e.target.value))}
            onBlur={applyPort}
            onKeyDown={(e) => e.key === "Enter" && applyPort()}
          />
        </label>
        {IS_ANDROID && (
          <>
            <label className="adv-row">
              <span>Background sync</span>
              <input
                type="checkbox"
                checked={bgSync}
                onChange={(e) => {
                  const v = e.target.checked;
                  setBgSync(v);
                  localStorage.setItem("sn.bgSync", v ? "1" : "0");
                  api
                    .setBgSync(v)
                    .then(() =>
                        props.onLog?.(`background sync: ${v ? "on" : "off"}`)
                      )
                      .catch((err) => props.onLog?.(`background sync: ${err}`));
                }}
              />
            </label>
            <div className="muted small">
              the process stays awake (a persistent notification in the
              status bar)
            </div>
          </>
        )}
        <div className="muted small">
           {p2pRunning
            ? `listening on port ${p2pPortLive}`
            : "P2P is not running (open a vault)"}
          </div>
        <button
          className="wide"
          disabled={busyRestart || !p2pRunning}
          onClick={doRestart}
        >
          Restart P2P
        </button>
        <div className="muted small" title={`${props.buildSha} · ${props.vaultPath}`}>
          build {props.buildSha}
        </div>
        <div className="muted small" title={props.vaultPath}>
          vault: {props.vaultPath}
        </div>
      </div>

      <h2>Log</h2>
      <div ref={logRef} className="log">
        {props.log.map((l, i) => (
          <div key={i} className="log-line">
            {l}
          </div>
        ))}
        {!props.log.length && <div className="muted">No events yet</div>}
      </div>
    </aside>
  );
}
