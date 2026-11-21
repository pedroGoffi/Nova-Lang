# Arrays, Structs, Classes, and Inheritance

## Arrays

```nova
let values: [i32; 3] = [10, 20, 12];
let first = values[0];
```

Array length is part of the type, and indexing starts at zero. See [`arrays`](../examples/features/arrays/src/main.nova).

## Structs

```nova
struct Point { x: i32, y: i32 }

impl Point {
    fn sum(self: &Point) -> i32 { return self.x + self.y; }
}

let point = new Point { x: 20, y: 22 };
```

`&Point` is a read-only borrow. Structs favor data composition. See [`structs`](../examples/features/structs/src/main.nova).

## Classes and constructors

```nova
class Entity { id: i32 }

impl Entity {
    static fn new() -> Self {
        return new Entity { id: 40 };
    }
}
```

Construction belongs inside the class implementation; users call `Entity::new()`.

## Inheritance and virtual methods

```nova
class Player : Entity { points: i32 }

impl Player {
    static fn new(points: i32) -> Self {
        return new Player { points: points };
    }

    fn add_points(self: &mut Player, points: i32) -> () {
        self.points += points;
        return;
    }

    override fn value(self: Player) -> i32 {
        return super.value() + self.points;
    }
}
```

Base initialization belongs to the constructor chain, not the call site. `&mut` permits mutation. A base `virtual` method may be replaced by `override`; `super` invokes the parent implementation. See [`classes`](../examples/features/classes/src/main.nova).
