#!/usr/bin/env node
/**
 * waymux-hook.mjs: Universal Agent Lifecycle Hook for waymux
 * 
 * Bridges CLI agents (Antigravity, Claude Code, OpenCode, Codex, Aider) with:
 * 1. acpd (Agent Control Protocol Daemon): High-throughput pane state, spinners, sound effects.
 * 2. lazygitrs: Automatic discovery and bidirectional review note injection.
 * 3. tmux: Active pane tracking and backdrop isolation coordination.
 */

import { execSync } from 'node:child_process';
import { writeFileSync, readFileSync, existsSync } from 'node:fs';

const LOG_FILE = process.env.WAYMUX_LOG_FILE || '/tmp/waymux-hook.log';
const ACPD_URL = process.env.ACPD_URL || 'http://127.0.0.1:4040/rpc';

function log(msg) {
  const line = `[${new Date().toISOString()}] ${msg}\n`;
  try {
    writeFileSync(LOG_FILE, line, { flag: 'a' });
  } catch (_) {}
}

function getActiveTmuxPane() {
  if (!process.env.TMUX) return null;
  try {
    return execSync('tmux display-message -p "#{pane_id}"', { encoding: 'utf-8' }).trim();
  } catch (_) {
    return null;
  }
}

async function notifyAcpd(method, params) {
  try {
    const res = await fetch(ACPD_URL, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ jsonrpc: '2.0', id: Date.now(), method, params })
    });
    return res.ok;
  } catch (_) {
    return false;
  }
}

async function main() {
  const event = process.argv[2] || 'prompt';
  const workspacePath = process.cwd();
  const safePath = workspacePath.replace(/[^a-zA-Z0-9]/g, '_');
  const pane = getActiveTmuxPane();

  if (pane) {
    try {
      writeFileSync(`/tmp/waymux-active-pane-${safePath}.txt`, pane);
      writeFileSync(`/tmp/agy-active-pane-${safePath}.txt`, pane); // Compatibility
    } catch (_) {}
  }

  log(`Event: ${event} | Pane: ${pane || 'none'} | Cwd: ${workspacePath}`);

  if (event === 'prompt' || event === 'start') {
    if (pane) {
      await notifyAcpd('update_agent_state', { pane_id: pane, state: 'working', details: 'Thinking' });
    }
  } else if (event === 'finish' || event === 'idle' || event === 'stop') {
    if (pane) {
      await notifyAcpd('update_agent_state', { pane_id: pane, state: 'idle', details: 'Ready' });
    }
  }
}

main().catch(e => log(`Error: ${e.message}`));
