import { onBeforeUnmount, ref } from 'vue'
import { api } from '@/lib/api'

/**
 * `GET /api/v1/meals/events` — the completion stream.
 *
 * Analysis is asynchronous and takes ~25s, so a view that shows a meal has to
 * be told when the loop lands. One `EventSource` is shared by every subscriber:
 * opening a second stream per view would multiply long-lived connections for no
 * benefit, since every frame carries the whole day state anyway.
 *
 * Frames arrive under the `meal` event name, each a JSON `MealEvent`.
 */
const EVENT_NAME = 'meal'

let source = null
let refCount = 0
const subscribers = new Set()

const connected = ref(false)
const lastEvent = ref(null)

function open() {
  if (source) return
  source = new EventSource(api.eventsUrl(), { withCredentials: true })

  source.addEventListener('open', () => {
    connected.value = true
  })

  source.addEventListener('error', () => {
    // EventSource reconnects on its own; reflect the gap rather than tearing
    // the stream down, so a proxy hiccup does not cost the user their updates.
    connected.value = false
  })

  source.addEventListener(EVENT_NAME, (message) => {
    let event
    try {
      event = JSON.parse(message.data)
    } catch {
      return
    }
    connected.value = true
    lastEvent.value = event
    for (const subscriber of subscribers) subscriber(event)
  })
}

function close() {
  if (!source) return
  source.close()
  source = null
  connected.value = false
}

/**
 * Subscribe to meal events for the lifetime of the calling component.
 *
 * @param {(event: object) => void} [handler] called for every frame
 */
export function useMealEvents(handler) {
  refCount += 1
  open()

  if (handler) subscribers.add(handler)

  onBeforeUnmount(() => {
    if (handler) subscribers.delete(handler)
    refCount -= 1
    if (refCount <= 0) {
      refCount = 0
      close()
    }
  })

  return { connected, lastEvent }
}
