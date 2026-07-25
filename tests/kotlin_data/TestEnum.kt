package pkg

enum class Direction {
    NORTH, SOUTH, EAST, WEST;

    fun opposite(): Direction = when(this) {
        NORTH -> SOUTH
        SOUTH -> NORTH
        EAST -> WEST
        WEST -> EAST
    }
}
