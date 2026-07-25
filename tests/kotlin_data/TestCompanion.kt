package pkg

class Counter private constructor(val count: Int) {
    companion object {
        fun create(initial: Int = 0): Counter = Counter(initial)
        const val MAX: Int = 100
    }

    fun increment(): Counter = Counter(count + 1)
}
