import { Link } from 'react-router-dom'

import type { mastodon } from '../masto.ts'
import { Avatar, AvatarFallback, AvatarImage } from '@/components/ui/avatar.tsx'

export function AccountRow({ account }: { account: mastodon.v1.Account }) {
  const name = account.displayName || account.username
  return (
    <Link
      to={`/@${account.acct}`}
      className="hover:bg-muted/50 flex items-center gap-3 rounded-lg p-2 no-underline"
    >
      <Avatar className="size-10 rounded-lg">
        <AvatarImage src={account.avatar} alt="" />
        <AvatarFallback>{name.slice(0, 1).toUpperCase()}</AvatarFallback>
      </Avatar>
      <div className="min-w-0">
        <div className="truncate font-medium">{name}</div>
        <div className="text-muted-foreground truncate text-sm">@{account.acct}</div>
      </div>
    </Link>
  )
}
