import DeviceList from '../components/DeviceList'

interface Props {
  token: string
}

export default function Devices({ token }: Props) {
  return (
    <div className="db-page">
      <div className="db-page-header">
        <h1 className="db-page-title">Devices</h1>
        <p className="db-page-subtitle">Manage the devices Goose can control.</p>
      </div>
      <div className="db-page-content">
        <DeviceList token={token} />
      </div>
    </div>
  )
}
