// Genesis API client with auth support

const BASE_URL = '/api'
const STORAGE_KEY = 'genesis_api_key'

// --- Auth helpers ---

export function setApiKey(key: string): void {
  localStorage.setItem(STORAGE_KEY, key)
}

export function clearApiKey(): void {
  localStorage.removeItem(STORAGE_KEY)
}

export function hasApiKey(): boolean {
  return localStorage.getItem(STORAGE_KEY) !== null
}

function getApiKey(): string | null {
  return localStorage.getItem(STORAGE_KEY)
}

// --- Error class ---

export class ApiError extends Error {
  readonly status: number

  constructor(status: number, message: string) {
    super(message)
    this.name = 'ApiError'
    this.status = status
  }
}

// --- Core fetch function ---

export async function apiFetch<T>(
  path: string,
  init: RequestInit = {},
): Promise<T> {
  const url = `${BASE_URL}${path}`

  const headers = new Headers(init.headers)

  // Set JSON content type for requests with a body
  if (init.body !== undefined && !headers.has('Content-Type')) {
    headers.set('Content-Type', 'application/json')
  }

  // Attach auth token if available
  const key = getApiKey()
  if (key) {
    headers.set('Authorization', `Bearer ${key}`)
  }

  const response = await fetch(url, { ...init, headers })

  if (!response.ok) {
    let message: string
    try {
      const body = await response.json()
      message = body.error ?? body.message ?? response.statusText
    } catch {
      message = response.statusText
    }
    throw new ApiError(response.status, message)
  }

  // Handle 204 No Content
  if (response.status === 204) {
    return undefined as unknown as T
  }

  return response.json() as Promise<T>
}

// --- Convenience methods ---

export const api = {
  get<T>(path: string, init?: Omit<RequestInit, 'method' | 'body'>): Promise<T> {
    return apiFetch<T>(path, { ...init, method: 'GET' })
  },

  post<T>(path: string, body?: unknown, init?: Omit<RequestInit, 'method' | 'body'>): Promise<T> {
    return apiFetch<T>(path, {
      ...init,
      method: 'POST',
      body: body !== undefined ? JSON.stringify(body) : undefined,
    })
  },

  put<T>(path: string, body?: unknown, init?: Omit<RequestInit, 'method' | 'body'>): Promise<T> {
    return apiFetch<T>(path, {
      ...init,
      method: 'PUT',
      body: body !== undefined ? JSON.stringify(body) : undefined,
    })
  },

  patch<T>(path: string, body?: unknown, init?: Omit<RequestInit, 'method' | 'body'>): Promise<T> {
    return apiFetch<T>(path, {
      ...init,
      method: 'PATCH',
      body: body !== undefined ? JSON.stringify(body) : undefined,
    })
  },

  delete<T>(path: string, init?: Omit<RequestInit, 'method' | 'body'>): Promise<T> {
    return apiFetch<T>(path, { ...init, method: 'DELETE' })
  },
}
