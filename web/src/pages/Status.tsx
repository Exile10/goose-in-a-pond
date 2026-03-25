import SystemStatus from '../components/SystemStatus'

interface Props {
  token: string
}

export default function Status({ token }: Props) {
  return (
    <div className="db-page">
      <div className="db-page-header">
        <h1 className="db-page-title">System Status</h1>
        <p className="db-page-subtitle">Health and information about this Goose In A Pond instance.</p>
      </div>
      <div className="db-page-content">
        <SystemStatus token={token} />
      </div>
    </div>
  )
}
