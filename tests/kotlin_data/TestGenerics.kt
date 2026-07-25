package pkg

class Container<T : Comparable<T>>(val items: List<T>) {
    fun <R> map(transform: (T) -> R): List<R> = items.map(transform)
    fun sorted(): List<T> = items.sorted()
}

interface Repository<out T, in K> {
    fun findById(id: K): T?
    fun findAll(): List<T>
}
