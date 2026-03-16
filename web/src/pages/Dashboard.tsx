import ChatWidget from '../components/ChatWidget'

interface Props {
  token: string
}

export default function Dashboard({ token }: Props) {
  return (
    <div className="db-chat-page">
      <ChatWidget token={token} />
    </div>
  )
}
