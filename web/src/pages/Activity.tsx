import ActivityFeed from '../components/ActivityFeed'

interface Props {
  token: string
}

export default function Activity({ token }: Props) {
  return (
    <div className="db-page">
      <div className="db-page-header">
        <h1 className="db-page-title">Recent Activity</h1>
        <p className="db-page-subtitle">A log of messages sent and device events.</p>
      </div>
      <div className="db-page-content">
        <ActivityFeed token={token} />
      </div>
    </div>
  )
}
