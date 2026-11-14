import type { ShellTab } from "../shell/types";
import type { NetworkSample, ServerStatus } from "../../types";

export function networkPatch(
  tab: ShellTab,
  sample: NetworkSample,
  rxBps: number,
  txBps: number,
): ShellTab {
  return {
    ...tab,
    networkSample: sample,
    networkRxBps: rxBps,
    networkTxBps: txBps,
    networkHistory: [...tab.networkHistory, { sampledAt: sample.sampledAt, rxBps, txBps }].slice(-40),
  };
}

/** One round of a server's streamed metrics (`server-status-sample`). */
export interface StatusSample {
  id: string;
  status: ServerStatus;
  network: NetworkSample;
  /** Whether `status` carries a fresh disk reading; disk is read less often. */
  hasDisk: boolean;
}

/**
 * A tab with a streamed sample applied: the live process rankings (which
 * arrive on their own stream) and the last disk reading are kept, and the
 * network rate comes from the change since the previous sample.
 */
export function applyStatusSample(tab: ShellTab, sample: StatusSample): ShellTab {
  if (tab.serverId !== sample.id) return tab;
  const previousStatus = tab.status;
  const status: ServerStatus = {
    ...sample.status,
    ...(previousStatus
      ? {
          topCpuProcesses: previousStatus.topCpuProcesses,
          topMemoryProcesses: previousStatus.topMemoryProcesses,
        }
      : {}),
    ...(!sample.hasDisk && previousStatus
      ? {
          diskUsedPercent: previousStatus.diskUsedPercent,
          diskUsedGb: previousStatus.diskUsedGb,
          diskTotalGb: previousStatus.diskTotalGb,
          diskMounts: previousStatus.diskMounts,
        }
      : {}),
  };
  const previous = tab.networkSample;
  const withStatus = { ...tab, status };
  if (previous && sample.network.sampledAt <= previous.sampledAt) return withStatus;
  // Counters reset when an interface goes away or the host reboots.
  const rate = (now: number, before: number, fallback: number) => {
    if (!previous) return fallback;
    const seconds = Math.max(0.2, sample.network.sampledAt - previous.sampledAt);
    return now >= before ? (now - before) / seconds : 0;
  };
  return networkPatch(
    withStatus,
    sample.network,
    rate(sample.network.rxBytes, previous?.rxBytes ?? 0, tab.networkRxBps),
    rate(sample.network.txBytes, previous?.txBytes ?? 0, tab.networkTxBps),
  );
}
