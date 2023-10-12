import .pkg.vars

a = "demo"
b = a
c = a

schema Name:
    name: str

schema Person:
    n: Name

p2 = Person {
    n: Name{
        name: a
    }
}

d = vars.My_var