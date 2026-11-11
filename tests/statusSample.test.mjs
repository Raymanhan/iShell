import assert from 'node:assert/strict';
import test from 'node:test';
import { applyStatusSample } from '../src/features/monitor/network.ts';

const tab = (patch = {}) => ({
  id: 't', serverId: 's', status: null, networkSample: null,
  networkRxBps: 0, networkTxBps: 0, networkHistory: [], ...patch,
});
const status = (patch = {}) => ({ id: 's', cpuPercent: 10, diskMounts: [], diskUsedPercent: null, ...patch });
const sample = (patch = {}) => ({
  id: 's', hasDisk: false, status: status(), network: { rxBytes: 0, txBytes: 0, sampledAt: 100 }, ...patch,
});

test('samples for other servers leave the tab alone', () => {
  const before = tab();
  assert.equal(applyStatusSample(before, sample({ id: 'other' })), before);
});

test('rounds without disk keep the last disk reading and the live process lists', () => {
  const mounts = [{ mountPoint: '/', usedPercent: 40 }];
  let current = applyStatusSample(tab(), sample({
    hasDisk: true, status: status({ diskMounts: mounts, diskUsedPercent: 40 }),
  }));
  current = { ...current, status: { ...current.status, topCpuProcesses: [{ pid: 1 }] } };
  current = applyStatusSample(current, sample({
    status: status({ cpuPercent: 55 }), network: { rxBytes: 0, txBytes: 0, sampledAt: 101 },
  }));
  assert.equal(current.status.cpuPercent, 55);
  assert.deepEqual(current.status.diskMounts, mounts);
  assert.equal(current.status.diskUsedPercent, 40);
  assert.deepEqual(current.status.topCpuProcesses, [{ pid: 1 }]);
});

test('network rates come from consecutive counters; resets read as zero', () => {
  let current = applyStatusSample(tab(), sample({ network: { rxBytes: 1000, txBytes: 500, sampledAt: 100 } }));
  current = applyStatusSample(current, sample({ network: { rxBytes: 3000, txBytes: 1500, sampledAt: 102 } }));
  assert.equal(current.networkRxBps, 1000);
  assert.equal(current.networkTxBps, 500);
  assert.equal(current.networkHistory.length, 2);
  // An older or repeated sample changes nothing about the rates.
  const same = applyStatusSample(current, sample({ network: { rxBytes: 9e9, txBytes: 0, sampledAt: 101 } }));
  assert.equal(same.networkRxBps, 1000);
  const reset = applyStatusSample(current, sample({ network: { rxBytes: 10, txBytes: 10, sampledAt: 103 } }));
  assert.equal(reset.networkRxBps, 0);
});
