package pkg

data class Vector(val x: Int, val y: Int) {
    operator fun plus(other: Vector): Vector = Vector(x + other.x, y + other.y)
    operator fun minus(other: Vector): Vector = Vector(x - other.x, y - other.y)
    operator fun times(scale: Int): Vector = Vector(x * scale, y * scale)
    operator fun unaryMinus(): Vector = Vector(-x, -y)
    operator fun get(index: Int): Int = if (index == 0) x else y
}

fun String.repeat(n: Int, separator: String = " "): String {
    return (1..n).joinToString(separator) { this }
}

fun List<Int>.sum(initial: Int = 0): Int {
    var acc = initial
    for (v in this) acc += v
    return acc
}
