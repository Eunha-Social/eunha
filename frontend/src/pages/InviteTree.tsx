import { useEffect, useState } from 'react'
import { Link } from 'react-router-dom'

import { getInviteTree, type InviteNode, type InviteTree } from '../eunha-api.ts'
import { getToken } from '../auth.ts'
import { TopBar } from '@/components/top-bar.tsx'
import { Avatar, AvatarFallback, AvatarImage } from '@/components/ui/avatar.tsx'

function TreeNode({ node, depth }: { node: InviteNode; depth: number }) {
  const name = node.display_name || node.username
  return (
    <li>
      <Link
        to={`/@${node.acct}`}
        className="hover:bg-muted/50 flex min-w-0 items-center gap-3 rounded-lg p-2 no-underline"
      >
        <Avatar className="size-8">
          <AvatarImage src={node.avatar} alt="" />
          <AvatarFallback>{name.slice(0, 1).toUpperCase()}</AvatarFallback>
        </Avatar>
        <div className="min-w-0">
          <div className="truncate font-medium">{name}</div>
          <div className="text-muted-foreground truncate text-sm">@{node.acct}</div>
        </div>
      </Link>
      {node.children.length > 0 && (
        // Children are indented and share a guide line, so the invite lineage
        // reads top-down.
        <ul className="border-muted ml-5 border-l pl-2">
          {node.children.map((child) => (
            <TreeNode key={child.id} node={child} depth={depth + 1} />
          ))}
        </ul>
      )}
    </li>
  )
}

export default function InviteTree() {
  const token = getToken()
  const [tree, setTree] = useState<InviteTree | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    if (!token) {
      setError('Sign in to view the invite tree.')
      return
    }
    getInviteTree(token)
      .then(setTree)
      .catch((e) => setError(String(e)))
  }, [token])

  return (
    <div className="page-frame">
      <TopBar />
      <h1 className="mb-2 text-lg font-bold">Invite tree</h1>
      {tree && (
        <p className="text-muted-foreground mb-3 text-sm">
          {tree.total} {tree.total === 1 ? 'member' : 'members'}
        </p>
      )}
      {error && <p className="text-destructive text-sm">{error}</p>}
      {!tree && !error && (
        <p className="text-muted-foreground text-sm">Loading…</p>
      )}
      {tree && tree.roots.length === 0 && (
        <p className="text-muted-foreground text-sm">No members yet.</p>
      )}
      {tree && tree.roots.length > 0 && (
        <ul className="space-y-1">
          {tree.roots.map((node) => (
            <TreeNode key={node.id} node={node} depth={0} />
          ))}
        </ul>
      )}
    </div>
  )
}
