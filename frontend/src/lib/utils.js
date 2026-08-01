import { clsx } from 'clsx'
import { twMerge } from 'tailwind-merge'

/** Merge conditional class lists, letting later Tailwind utilities win. */
export function cn(...inputs) {
  return twMerge(clsx(inputs))
}
