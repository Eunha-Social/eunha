import { useEffect, useState } from 'react'
import { useNavigate, useSearchParams } from 'react-router-dom'
import { completeLogin } from '../auth.ts'

export default function Callback() {
  const [params] = useSearchParams()
  const navigate = useNavigate()
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    const code = params.get('code')
    if (!code) {
      setError(params.get('error_description') ?? 'No authorization code returned.')
      return
    }
    completeLogin(code)
      .then(() => navigate('/', { replace: true }))
      .catch((e) => setError(String(e)))
  }, [params, navigate])

  return (
    <div className="app">
      <p className={error ? 'error' : 'muted'}>
        {error ?? 'Signing you in…'}
      </p>
    </div>
  )
}
