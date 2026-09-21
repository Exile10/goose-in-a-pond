import React, { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api/PondApiClient';

/** Household activation stays on the Pond's protected local dashboard. */
export function RemoteAccess() {
  const { t } = useTranslation();
  const [control, setControl] = useState('');
  const [enrollment, setEnrollment] = useState('');
  const [identity, setIdentity] = useState<{ household: string; publicKey: string } | null>(null);
  const [state, setState] = useState('connecting');
  const [requests, setRequests] = useState<{ id: string; device: string; approved: boolean }[]>([]);
  const [reviewFailed, setReviewFailed] = useState(false);
  const [busy, setBusy] = useState(false);
  const generation = useRef(0);
  useEffect(() => {
    const current = ++generation.current;
    void api.remoteStatus().then((status) => {
      if (current === generation.current) setState(status.state === 'Running' ? 'enabled' : status.state === 'Stopped' ? 'local' : 'failed');
    }).catch(() => { if (current === generation.current) setState('failed'); });
    return () => { generation.current++; };
  }, []);
  useEffect(() => {
    let active = true;
    let timer: ReturnType<typeof setTimeout>;
    const poll = async () => {
      try {
        const pending = await api.remoteRecoveryRequests();
        if (active) { setRequests(pending); setReviewFailed(false); }
      } catch { if (active) setReviewFailed(true); }
      if (active) timer = setTimeout(() => void poll(), 5000);
    };
    void poll();
    return () => { active = false; clearTimeout(timer); };
  }, []);
  const approve = async (id: string) => {
    setBusy(true);
    try {
      await api.approveRemoteRecovery(id);
      setRequests((pending) => pending.map((request) => request.id === id ? { ...request, approved: true } : request));
      setReviewFailed(false);
    } catch { setReviewFailed(true); console.warn('[remote] recovery approval failed'); }
    finally { setBusy(false); }
  };
  const run = async (operation: 'identity' | 'enable' | 'disable') => {
    const current = ++generation.current;
    setBusy(true);
    try {
      if (operation === 'identity') {
        const prepared = await api.prepareRemoteIdentity();
        if (current !== generation.current) return;
        setIdentity(prepared);
        setState('provision');
      } else if (operation === 'disable') {
        await api.disableRemoteAccess();
        if (current !== generation.current) return;
        setState('local');
      } else {
        // Empty means the hosted coordinator, which the server fills in when it
        // enables. Only validate what the household actually chose.
        for (const value of [control, enrollment].filter((entry) => entry !== '')) {
          const url = new URL(value);
          if (url.protocol !== 'https:' || url.username || url.password || url.search || url.hash || url.pathname !== '/') throw new Error('invalid origin');
        }
        await api.enableRemoteAccess({ enabled: true, controlUrl: control, enrollmentUrl: enrollment });
        setState('connecting');
        let registered = false;
        for (let attempt = 0; attempt < 30; attempt++) {
          if (current !== generation.current) return;
          const status = await api.remoteStatus();
          if (current !== generation.current) return;
          if (status.state === 'Running') { setState('enabled'); return; }
          if (status.authUrl && !registered) {
            registered = true;
            // Never retry registration automatically after an ambiguous response.
            await api.registerRemotePond();
          }
          await new Promise<void>((resolve) => setTimeout(resolve, 2000));
        }
        throw new Error('activation timeout');
      }
    } catch {
      if (current === generation.current) setState('failed');
      console.warn('[remote] local activation operation failed');
    } finally { if (current === generation.current) setBusy(false); }
  };
  // One primary action that matches the current state. Showing both an enable and
  // a disable control at once is what made the panel read as "not enabled" while
  // it was running.
  const on = state === 'enabled';
  const settling = state === 'connecting' || state === 'provision';
  const field: React.CSSProperties = { display: 'grid', gap: 4 };
  const input: React.CSSProperties = { width: '100%', boxSizing: 'border-box' };
  return <section aria-labelledby="remote-access-title" style={{ marginTop: 32, display: 'grid', gap: 16 }}>
    <div style={{ display: 'grid', gap: 6 }}>
      <h2 id="remote-access-title" style={{ margin: 0 }}>{t('remote.title')}</h2>
      <p style={{ margin: 0 }}>{t('remote.description')}</p>
    </div>
    <p role="status" style={{ margin: 0, fontWeight: 600 }}>{t(`remote.${state}`)}</p>
    <div>
      <button disabled={busy || settling} onClick={() => void run(on ? 'disable' : 'enable')}>
        {t(on ? 'remote.disable' : 'remote.enable')}
      </button>
    </div>
    <details>
      <summary>{t('remote.advanced')}</summary>
      <div style={{ display: 'grid', gap: 12, marginTop: 12 }}>
        <p style={{ margin: 0 }}>{t('remote.advancedDescription')}</p>
        <label style={field}>
          <span>{t('remote.coordinator')}</span>
          <input style={input} type="url" inputMode="url" placeholder="https://control.example" value={control} onChange={(e) => setControl(e.target.value)} disabled={busy || on} />
        </label>
        <label style={field}>
          <span>{t('remote.enrollment')}</span>
          <input style={input} type="url" inputMode="url" placeholder="https://enroll.example" value={enrollment} onChange={(e) => setEnrollment(e.target.value)} disabled={busy || on} />
        </label>
        <div>
          <button disabled={busy} onClick={() => void run('identity')}>{t('remote.identity')}</button>
        </div>
        {identity && <div style={{ overflowWrap: 'anywhere', display: 'grid', gap: 4 }}>
          <p style={{ margin: 0 }}>{t('remote.provision')}</p>
          <dl style={{ display: 'grid', gap: 4, margin: 0 }}>
            <dt style={{ fontWeight: 600 }}>{t('remote.household')}</dt><dd style={{ margin: 0 }}>{identity.household}</dd>
            <dt style={{ fontWeight: 600 }}>{t('remote.publicKey')}</dt><dd style={{ margin: 0 }}>{identity.publicKey}</dd>
          </dl>
        </div>}
      </div>
    </details>
    <div style={{ display: 'grid', gap: 6 }}>
      <h3 style={{ margin: 0 }}>{t('remote.recoveryTitle')}</h3>
      <p style={{ margin: 0 }}>{t('remote.recoveryDescription')}</p>
      {reviewFailed && <p role="alert" style={{ margin: 0 }}>{t('remote.reviewFailed')}</p>}
      {requests.map((request) => <div key={request.id} style={{ overflowWrap: 'anywhere', display: 'grid', gap: 4 }}>
        <p style={{ margin: 0 }}>{t('remote.reviewCode', { id: request.id })}</p>
        <div>
          <button disabled={busy || request.approved} onClick={() => void approve(request.id)}>{t(request.approved ? 'remote.reviewApproved' : 'remote.reviewApprove')}</button>
        </div>
      </div>)}
    </div>
  </section>;
}
