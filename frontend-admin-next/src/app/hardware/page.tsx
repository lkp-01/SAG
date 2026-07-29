"use client";

import { useEffect, useMemo, useState } from "react";
import { TopBar } from "@/components/app-shell/TopBar";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { pickScalar, promQuery } from "@/lib/prom";

type Stat = {
  cpu: number | null;
  load1: number | null;
  memUsed: number | null;
  memTotal: number | null;
  diskUsed: number | null;
  diskTotal: number | null;
  rxBps: number | null;
  txBps: number | null;
};

export default function HardwarePage() {
  const [stat, setStat] = useState<Stat>({
    cpu: null,
    load1: null,
    memUsed: null,
    memTotal: null,
    diskUsed: null,
    diskTotal: null,
    rxBps: null,
    txBps: null
  });
  const [err, setErr] = useState<string>("");

  const refreshEveryMs = useMemo(() => Number(process.env.NEXT_PUBLIC_REFRESH_MS ?? "5000"), []);

  useEffect(() => {
    let cancelled = false;

    async function runOnce() {
      try {
        // CPU 使用率（1 - idle），跨 core 求平均。
        const qCpu = '1 - avg(rate(node_cpu_seconds_total{mode="idle"}[1m]))';
        const qMemTotal = "node_memory_MemTotal_bytes";
        const qMemAvail = "node_memory_MemAvailable_bytes";

        // 磁盘：按 rootfs / 或最大分区展示一个“总体”值（先用 max 做近似）。
        const qDiskTotal = 'max(node_filesystem_size_bytes{fstype!~"tmpfs|overlay",mountpoint!~"/var/lib/docker.*"})';
        const qDiskAvail = 'max(node_filesystem_avail_bytes{fstype!~"tmpfs|overlay",mountpoint!~"/var/lib/docker.*"})';

        const qLoad1 = "node_load1";
        const qRx = 'sum(rate(node_network_receive_bytes_total{device!~"lo"}[1m]))';
        const qTx = 'sum(rate(node_network_transmit_bytes_total{device!~"lo"}[1m]))';

        const [rCpu, rLoad1, rMemT, rMemA, rDiskT, rDiskA, rRx, rTx] = await Promise.all([
          promQuery(qCpu),
          promQuery(qLoad1),
          promQuery(qMemTotal),
          promQuery(qMemAvail),
          promQuery(qDiskTotal),
          promQuery(qDiskAvail),
          promQuery(qRx),
          promQuery(qTx)
        ]);
        if (cancelled) return;

        const cpu = pickScalar(rCpu, NaN);
        const load1 = pickScalar(rLoad1, NaN);
        const memTotal = pickScalar(rMemT, NaN);
        const memAvail = pickScalar(rMemA, NaN);
        const diskTotal = pickScalar(rDiskT, NaN);
        const diskAvail = pickScalar(rDiskA, NaN);
        const rx = pickScalar(rRx, NaN);
        const tx = pickScalar(rTx, NaN);

        setStat({
          cpu: Number.isFinite(cpu) ? cpu : null,
          load1: Number.isFinite(load1) ? load1 : null,
          memUsed: Number.isFinite(memTotal) && Number.isFinite(memAvail) ? memTotal - memAvail : null,
          memTotal: Number.isFinite(memTotal) ? memTotal : null,
          diskUsed: Number.isFinite(diskTotal) && Number.isFinite(diskAvail) ? diskTotal - diskAvail : null,
          diskTotal: Number.isFinite(diskTotal) ? diskTotal : null,
          rxBps: Number.isFinite(rx) ? rx : null,
          txBps: Number.isFinite(tx) ? tx : null
        });
        setErr("");
      } catch (e) {
        if (cancelled) return;
        setErr(String(e));
        setStat({
          cpu: null,
          load1: null,
          memUsed: null,
          memTotal: null,
          diskUsed: null,
          diskTotal: null,
          rxBps: null,
          txBps: null
        });
      }
    }

    runOnce();
    const iv = window.setInterval(runOnce, Number.isFinite(refreshEveryMs) ? refreshEveryMs : 5000);
    return () => {
      cancelled = true;
      window.clearInterval(iv);
    };
  }, [refreshEveryMs]);

  function fmtBytes(v: number | null) {
    if (v == null) return "—";
    const gb = v / 1024 / 1024 / 1024;
    if (gb >= 1) return `${gb.toFixed(1)} GiB`;
    const mb = v / 1024 / 1024;
    return `${mb.toFixed(0)} MiB`;
  }

  function pct(used: number | null, total: number | null) {
    if (used == null || total == null || total <= 0) return "—";
    return `${((used / total) * 100).toFixed(1)}%`;
  }

  function fmtBps(v: number | null) {
    if (v == null) return "—";
    const kb = v / 1024;
    if (kb < 1024) return `${kb.toFixed(0)} KiB/s`;
    const mb = kb / 1024;
    if (mb < 1024) return `${mb.toFixed(1)} MiB/s`;
    const gb = mb / 1024;
    return `${gb.toFixed(2)} GiB/s`;
  }

  return (
    <>
      <TopBar title="硬件状态" />
      <div className="flex-1 p-4 md:p-6">
        <div className="grid grid-cols-1 gap-4 md:grid-cols-3">
          <Card>
            <CardHeader>
              <CardTitle>CPU</CardTitle>
            </CardHeader>
            <CardContent className="space-y-1 text-sm text-muted-foreground">
              <div>使用率：{stat.cpu == null ? "—" : `${(stat.cpu * 100).toFixed(1)}%`}</div>
              <div>Load1：{stat.load1 == null ? "—" : stat.load1.toFixed(2)}</div>
            </CardContent>
          </Card>

          <Card>
            <CardHeader>
              <CardTitle>内存</CardTitle>
            </CardHeader>
            <CardContent className="space-y-1 text-sm text-muted-foreground">
              <div>
                使用：{fmtBytes(stat.memUsed)} / {fmtBytes(stat.memTotal)}（{pct(stat.memUsed, stat.memTotal)}）
              </div>
            </CardContent>
          </Card>

          <Card>
            <CardHeader>
              <CardTitle>磁盘（概览）</CardTitle>
            </CardHeader>
            <CardContent className="space-y-1 text-sm text-muted-foreground">
              <div>
                使用：{fmtBytes(stat.diskUsed)} / {fmtBytes(stat.diskTotal)}（{pct(stat.diskUsed, stat.diskTotal)}）
              </div>
              <div className="text-xs text-muted-foreground">本页数据来自 node_exporter（Prometheus job: `node`）。</div>
              {err ? <div className="text-xs text-destructive">Prometheus 未就绪：{err}</div> : null}
            </CardContent>
          </Card>

          <Card>
            <CardHeader>
              <CardTitle>网络吞吐</CardTitle>
            </CardHeader>
            <CardContent className="space-y-1 text-sm text-muted-foreground">
              <div>接收：{fmtBps(stat.rxBps)}</div>
              <div>发送：{fmtBps(stat.txBps)}</div>
            </CardContent>
          </Card>
        </div>
      </div>
    </>
  );
}

