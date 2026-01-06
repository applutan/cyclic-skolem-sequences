KCounterFast := function(n)
    local nvx, pts, sols, dhg, Backtrack;
    
    # Calculate number of vertices (2n for even case, 4n+2 or similar for odd)
    # NOTE: The input 'n' here is the number of CHORDS. 
    # Adjust this logic if you pass the sequence index 'k' instead.
    nvx := 2*n; 
    
    sols := [];
    
    # Define recursion locally so it captures 'sols' safely
    Backtrack := function(mus, mln, cps, fnd)
        local x, y, l;
    
        # Base case: All n chords placed
        if fnd = n then
            Add(sols, Set(cps));
            return;
        fi;
    
        # Find first unused vertex
        x := Position(mus, false);
    
        # Try to pair x with every other unused vertex y
        for y in [x+1 .. nvx] do
            if mus[y] = false then
                # Calculate chord length
                l := Minimum(y - x, nvx - (y - x));
    
                # PRUNING: Only proceed if this length hasn't been used yet
                if mln[l] = false then
                    # Mark used
                    mus[x] := true; mus[y] := true;
                    mln[l] := true;
                    Add(cps, Set([x,y]));
    
                    # Recurse
                    Backtrack(mus, mln, cps, fnd + 1);
    
                    # Backtrack (Unmark)
                    Remove(cps);
                    mln[l] := false;
                    mus[y] := false; mus[x] := false;
                fi;
            fi;
        od;
    end;

    # Launch recursion
    # mus: boolean list of used vertices (size 2n)
    # mln: boolean list of used chord lengths (size n)
    Backtrack(ListWithIdenticalEntries(nvx, false), ListWithIdenticalEntries(n, false), [], 0);
    
    if IsEmpty(sols) then return 0; fi;
    
    # Construct Dihedral Group D_2n acting on the set of pairs
    dhg := Group(PermList(Concatenation([2..nvx], [1])), Product(List([1..n], i -> (i, nvx-i+1))));
    
    return Size(OrbitsDomain(dhg, sols, OnSetsSets));
end;
