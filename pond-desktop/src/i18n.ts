import i18n from 'i18next';
import { initReactI18next } from 'react-i18next';

void i18n.use(initReactI18next).init({
  lng: 'en', fallbackLng: 'en', interpolation: { escapeValue: false },
  resources: { en: { translation: { remote: {
    title: 'Remote access',
    recoveryTitle: 'Phone recovery requests',
    recoveryDescription: 'After pairing the phone locally again, match its request identifier here before approving. Approval expires after two minutes and must be completed by that phone on your home network.',
    reviewCode: 'Request: {{id}}', reviewApprove: 'Approve this phone recovery', reviewApproved: 'Approved; waiting for phone',
    reviewFailed: 'Recovery requests could not be loaded or approved. Check the local connection and retry.',
    description: 'Approve remote access from this Pond. Local-only operation does not contact the coordination service. Pilot setup requires operator provisioning.',
    identity: 'Prepare household identity', household: 'Household identifier', publicKey: 'Enrollment public key',
    provision: 'Give this public identity to your pilot operator before enabling remote access. Keep the private identity and its backup on your Pond.',
    coordinator: 'Headscale HTTPS origin', enrollment: 'Enrollment HTTPS origin',
    enable: 'Approve and enable remote access', disable: 'Keep local only',
    local: 'Remote access is disabled.', connecting: 'Registering this Pond with the coordinator.',
    enabled: 'Remote access is enabled. Paired phones can now request enrollment on your LAN.',
    failed: 'Remote access did not complete. Check the local server logs and pilot provisioning before retrying.',
  }, pairing: {
    unavailable: 'HTTPS pairing is unavailable. Check the Pond server logs.',
    fingerprint: 'Public-key fingerprint', address: 'HTTPS address',
    manual: 'For manual pairing, enter this address, fingerprint, and pairing code on your phone while connected to the same LAN.',
  } } } },
});
export default i18n;
